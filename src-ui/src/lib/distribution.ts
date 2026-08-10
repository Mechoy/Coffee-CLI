/**
 * Local distribution identity. Keep the semantic application version in
 * Cargo/Tauri as plain x.y.z; this label and release namespace distinguish
 * Mechoy builds without confusing native package managers or version checks.
 */
export const DISTRIBUTION_LABEL = 'Mechoy Build';
export const DISTRIBUTION_REPOSITORY_URL = 'https://github.com/Mechoy/Coffee-CLI';
export const DISTRIBUTION_RELEASES_URL = `${DISTRIBUTION_REPOSITORY_URL}/releases`;
// CI updates this marker only after the matching release is published. Keeping
// it on raw GitHub avoids unauthenticated Releases API rate limits while still
// letting the installer derive a pinned tag and asset name.
export const DISTRIBUTION_VERSION_MANIFEST_URL =
  'https://raw.githubusercontent.com/Mechoy/Coffee-CLI/main/Web-Home/mechoy-version.json';

/** Compare the numeric app versions accepted by Tauri and the install scripts. */
export function isNewerDistributionVersion(remote: string, local: string): boolean {
  const numericVersion = /^\d+\.\d+\.\d+$/;
  if (!numericVersion.test(remote) || !numericVersion.test(local)) {
    return false;
  }

  const remoteParts = remote.split('.').map(Number);
  const localParts = local.split('.').map(Number);

  for (let index = 0; index < 3; index += 1) {
    if (remoteParts[index] > localParts[index]) return true;
    if (remoteParts[index] < localParts[index]) return false;
  }
  return false;
}
