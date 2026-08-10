// Cloudflare Pages Function for the Mechoy distribution site.
// Uses env.ASSETS to serve CF Pages static files directly.
//
// Routes:
//   /download/<platform>   → proxy GitHub Release assets
//   /*                     → CF Pages static files (env.ASSETS)

const REPO = "Mechoy/Coffee-CLI"
const VERSION_MANIFEST_URL = "https://raw.githubusercontent.com/Mechoy/Coffee-CLI/main/Web-Home/mechoy-version.json"
const VERSION = /^(\d+\.\d+\.\d+)$/

// A custom tag and the exact staged filename are both required. This keeps a
// mistaken upstream-style release in the fork from replacing a Mechoy build.
const PLATFORM_ASSET_SUFFIXES = {
  "windows": "Windows_x64-setup.exe",
  "windows-msi": "Windows_x64.msi",
  "macos-arm": "macOS_arm64.dmg",
  "macos-intel": "macOS_x64.dmg",
  "linux-deb": "Linux_x64.deb",
  "linux-rpm": "Linux_x64.rpm",
  "linux-appimage": "Linux_x64.AppImage",
  "linux-arm64-deb": "Linux_arm64.deb",
  "linux-arm64-rpm": "Linux_arm64.rpm",
  "linux-arm64-appimage": "Linux_arm64.AppImage",
}

async function getLatestAssets(env) {
  // A new cache namespace prevents an old API-shaped entry from being reused
  // after the fork moved to its release-published version marker.
  const cacheKey = "mechoy-latest-release-v2"
  // Separate "last known good" key with no TTL. Used as a stale
  // fallback when the marker request fails (network outage).
  const stableKey = "mechoy-latest-release-stable-v2"
  if (env.KV) {
    const cached = await env.KV.get(cacheKey)
    if (cached) return JSON.parse(cached)
  }

  let res
  try {
    res = await fetch(VERSION_MANIFEST_URL, {
      headers: { "User-Agent": "CoffeeCLI-Mechoy-Worker" }
    })
  } catch (e) {
    if (env.KV) {
      const stale = await env.KV.get(stableKey)
      if (stale) return JSON.parse(stale)
    }
    throw e
  }
  if (!res.ok) {
    if (env.KV) {
      const stale = await env.KV.get(stableKey)
      if (stale) return JSON.parse(stale)
    }
    throw new Error(`Mechoy version marker ${res.status}`)
  }

  const marker = await res.json()
  const versionMatch = typeof marker.version === "string" && VERSION.exec(marker.version)
  if (!versionMatch) {
    throw new Error("version marker does not contain a Mechoy version")
  }
  const version = versionMatch[1]
  const assets = {}
  for (const [platform, suffix] of Object.entries(PLATFORM_ASSET_SUFFIXES)) {
    const expectedName = `Coffee.CLI_Mechoy_${version}_${suffix}`
    assets[platform] = {
      url: `https://github.com/${REPO}/releases/download/mechoy-v${version}/${expectedName}`,
      name: expectedName,
      version
    }
  }

  if (env.KV) {
    const payload = JSON.stringify(assets)
    await env.KV.put(cacheKey, payload, { expirationTtl: 3600 })
    // Stable copy has no TTL — only ever overwritten by a successful
    // fetch, never expires on its own. Worst case during a long
    // outage: users see the previous release until GitHub recovers.
    await env.KV.put(stableKey, payload)
  }
  return assets
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url)
    const { pathname } = url

    // ── /download/<platform> ─────────────────────────────────────────────────
    const dlMatch = pathname.match(/^\/download\/([a-z0-9-]+)$/)
    if (dlMatch) {
      const platform = dlMatch[1]
      if (!PLATFORM_ASSET_SUFFIXES[platform]) {
        return new Response(
          `Unknown platform "${platform}". Available: ${Object.keys(PLATFORM_ASSET_SUFFIXES).join(", ")}`,
          { status: 400 }
        )
      }

      let assets
      try {
        assets = await getLatestAssets(env)
      } catch (e) {
        return new Response(`Failed to fetch release info: ${e.message}`, { status: 502 })
      }

      const asset = assets[platform]
      if (!asset) {
        return new Response(`No asset found for "${platform}"`, { status: 404 })
      }

      const fileRes = await fetch(asset.url, {
        headers: { "User-Agent": "CoffeeCLI-Mechoy-Worker" }
      })
      if (!fileRes.ok) {
        return new Response(`Failed to download ${asset.name}: ${fileRes.status}`, { status: 502 })
      }
      return new Response(fileRes.body, {
        status: 200,
        headers: {
          "Content-Type": "application/octet-stream",
          "Content-Disposition": `attachment; filename="${asset.name}"`,
          "Content-Length": fileRes.headers.get("Content-Length") || "",
          "X-Coffee-Version": asset.version,
          "Cache-Control": "no-store",
        }
      })
    }

    // ── /lang-packs/<path> → 410 Gone ────────────────────────────────────────
    // Language pack infrastructure was retired. Intercept at the Worker so
    // edge-cached 200 responses from the pre-deletion era are replaced. The
    // 410 status tells HTTP clients the resource is permanently gone.
    if (pathname.startsWith("/lang-packs/")) {
      return new Response(
        "Coffee CLI language packs have been retired.\n" +
        "See Coffee 101 for installation and usage guides:\n" +
        "  https://coffeecli.com/courses/claude-code\n",
        {
          status: 410,
          headers: {
            "Content-Type": "text/plain; charset=utf-8",
            "Cache-Control": "no-store",
          }
        }
      )
    }

    // ── everything else → CF Pages static files ──────────────────────────────
    return env.ASSETS.fetch(request)
  }
}
