// pty-event-bus.ts — Singleton Tauri event router for PTY events.
//
// Before this existed, every TierTerminal instance called listen() for each
// PTY event type. Tauri multicasts events to every subscription, so with N
// tabs open, every PTY chunk triggered N callbacks — (N-1) of them just did
// an ID check and early-returned.
//
// This module registers exactly ONE listener per event type at the process
// level, keeps a Map<sessionId, handler>, and routes incoming events to the
// right handler by ID. N-tab fan-out collapses to O(1) map lookup per event.
//
// Usage:
//   const unsub = await subscribeTerminalEvents(sessionId, {
//     onOutput: (data) => { ... },
//     onStatus: (running, exit_code) => { ... },
//     onCwd:    (cwd) => { ... },
//   });
//   // later, on unmount:
//   unsub();

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

interface OutputEventPayload { id: string; data: string; }
interface StatusEventPayload { id: string; running: boolean; exit_code: number | null; }
interface CwdEventPayload { id: string; cwd: string; }
interface ExitEventPayload { id: string; exit_code: number; }
export interface PeerTaskEventPayload {
  job_id: string;
  source_id: string;
  target_id: string;
  status: 'completed' | 'failed';
  error?: string;
}

type OutputHandler = (data: string) => void;
type StatusHandler = (running: boolean, exitCode: number | null) => void;
type CwdHandler = (cwd: string) => void;
type ExitHandler = (exitCode: number) => void;
type PeerTaskHandler = (event: PeerTaskEventPayload) => void;

interface TerminalEventHandlers {
  onOutput?: OutputHandler;
  onStatus?: StatusHandler;
  onCwd?: CwdHandler;
  /** Fires when the Rust child-watcher thread detects the spawned process has
   *  actually died (via child.wait()). Distinct from onStatus which fires
   *  after the reader thread sees EOF — onExit may arrive earlier, and with
   *  the real exit code instead of the hardcoded 0 in the status event. */
  onExit?: ExitHandler;
  /** Structured peer completion routed by target pane id. Unlike terminal
   *  output, this event is emitted only after Rust stores the task result. */
  onPeerTask?: PeerTaskHandler;
}

const outputHandlers = new Map<string, OutputHandler>();
const statusHandlers = new Map<string, StatusHandler>();
const cwdHandlers = new Map<string, CwdHandler>();
const exitHandlers = new Map<string, ExitHandler>();
const peerTaskHandlers = new Map<string, PeerTaskHandler>();
// Completion is control data, so a short component remount must not drop it.
// Keep one copy per job until the destination pane registers again. Rust also
// stores the full result; this cache only preserves the wake-up notification.
const pendingPeerTasks = new Map<string, Map<string, PeerTaskEventPayload>>();
const MAX_PENDING_PEER_TASKS_PER_PANE = 64;
const MAX_PENDING_PEER_TASK_PANES = 32;

function retainPeerTask(event: PeerTaskEventPayload): void {
  let pending = pendingPeerTasks.get(event.target_id);
  if (!pending) {
    while (pendingPeerTasks.size >= MAX_PENDING_PEER_TASK_PANES) {
      const oldestPane = pendingPeerTasks.keys().next().value;
      if (!oldestPane) break;
      pendingPeerTasks.delete(oldestPane);
    }
    pending = new Map();
    pendingPeerTasks.set(event.target_id, pending);
  }
  pending.set(event.job_id, event);
  while (pending.size > MAX_PENDING_PEER_TASKS_PER_PANE) {
    const oldest = pending.keys().next().value;
    if (!oldest) break;
    pending.delete(oldest);
  }
}

let globalUnlisteners: UnlistenFn[] | null = null;
let initPromise: Promise<void> | null = null;

async function ensureInit(): Promise<void> {
  if (globalUnlisteners !== null) return;
  if (initPromise) return initPromise;

  initPromise = (async () => {
    const unOutput = await listen<OutputEventPayload>('tier-terminal-output', (event) => {
      const handler = outputHandlers.get(event.payload.id);
      if (handler) handler(event.payload.data);
    });
    const unStatus = await listen<StatusEventPayload>('tier-terminal-status', (event) => {
      const handler = statusHandlers.get(event.payload.id);
      if (handler) handler(event.payload.running, event.payload.exit_code);
    });
    const unCwd = await listen<CwdEventPayload>('tier-terminal-cwd', (event) => {
      const handler = cwdHandlers.get(event.payload.id);
      if (handler) handler(event.payload.cwd);
    });
    const unExit = await listen<ExitEventPayload>('tier-terminal-exit', (event) => {
      const handler = exitHandlers.get(event.payload.id);
      if (handler) handler(event.payload.exit_code);
    });
    const unPeerTask = await listen<PeerTaskEventPayload>('multi-agent-task-complete', (event) => {
      const handler = peerTaskHandlers.get(event.payload.target_id);
      if (handler) {
        handler(event.payload);
      } else {
        retainPeerTask(event.payload);
      }
    });
    globalUnlisteners = [unOutput, unStatus, unCwd, unExit, unPeerTask];
  })();

  return initPromise;
}

/**
 * Subscribe to PTY events for a specific session.
 * Returns an unsubscribe function. Safe to call before or after the global
 * Tauri listeners are initialized — initialization is lazy and shared.
 *
 * Only one handler per (session, event type) is supported. Calling subscribe
 * again for the same session overwrites previous handlers for that session.
 */
export async function subscribeTerminalEvents(
  sessionId: string,
  handlers: TerminalEventHandlers,
): Promise<() => void> {
  await ensureInit();

  // Capture references to the handlers we just registered.
  // The unsub function must only remove OUR handlers, not a newer mount's.
  const myOutput = handlers.onOutput;
  const myStatus = handlers.onStatus;
  const myCwd = handlers.onCwd;
  const myExit = handlers.onExit;
  const myPeerTask = handlers.onPeerTask;

  if (myOutput) outputHandlers.set(sessionId, myOutput);
  if (myStatus) statusHandlers.set(sessionId, myStatus);
  if (myCwd) cwdHandlers.set(sessionId, myCwd);
  if (myExit) exitHandlers.set(sessionId, myExit);
  if (myPeerTask) {
    peerTaskHandlers.set(sessionId, myPeerTask);
    const pending = pendingPeerTasks.get(sessionId);
    if (pending) {
      pendingPeerTasks.delete(sessionId);
      for (const event of pending.values()) myPeerTask(event);
    }
  }

  return () => {
    // Only delete if the registered handler is still ours.
    // React Strict Mode double-mounts with the same sessionId: the second
    // mount overwrites the Map entry, so the first mount's stale unsub must
    // NOT blow away the second mount's live handler.
    if (myOutput && outputHandlers.get(sessionId) === myOutput) outputHandlers.delete(sessionId);
    if (myStatus && statusHandlers.get(sessionId) === myStatus) statusHandlers.delete(sessionId);
    if (myCwd && cwdHandlers.get(sessionId) === myCwd) cwdHandlers.delete(sessionId);
    if (myExit && exitHandlers.get(sessionId) === myExit) exitHandlers.delete(sessionId);
    if (myPeerTask && peerTaskHandlers.get(sessionId) === myPeerTask) peerTaskHandlers.delete(sessionId);
  };
}
