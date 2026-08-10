// pty-event-bus.ts — Singleton Tauri event router for PTY events.
//
// Before this existed, every TierTerminal instance called listen() for each
// PTY event type. Tauri multicasts events to every subscription, so with N
// tabs open, every PTY chunk triggered N callbacks — (N-1) of them just did
// an ID check and early-returned.
//
// This module registers exactly ONE listener per event type at the process
// level, keeps a Map<(sessionId, runId), handler>, and routes incoming events
// to the right terminal run. A pane may restart in place before an older child
// has fully exited, so sessionId alone is not enough to distinguish delayed
// output/status events from the current process. N-tab fan-out collapses to
// O(1) map lookup per event.
//
// Usage:
//   const unsub = await subscribeTerminalEvents(sessionId, runId, {
//     onOutput: (data) => { ... },
//     onStatus: (running, exit_code) => { ... },
//     onCwd:    (cwd) => { ... },
//   });
//   // later, on unmount:
//   unsub();

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

interface OutputEventPayload { id: string; run_id: string; data: string; }
interface StatusEventPayload { id: string; run_id: string; running: boolean; exit_code: number | null; }
interface CwdEventPayload { id: string; run_id: string; cwd: string; }
interface ExitEventPayload { id: string; run_id: string; exit_code: number; }
export interface PeerTaskEventPayload {
  job_id: string;
  source_id: string;
  source_run_id: string;
  target_id: string;
  target_run_id: string;
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
  /** Structured peer completion routed by target pane and its run ID. Unlike
   *  terminal output, this event is emitted only after Rust stores the task
   *  result. */
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
const MAX_PENDING_PEER_TASKS_PER_TARGET = 64;
const MAX_PENDING_PEER_TASK_TARGETS = 32;

function terminalHandlerKey(sessionId: string, runId: string): string {
  // NUL cannot occur in a UUID or Coffee session id, so this cannot collide
  // with another pair through simple concatenation.
  return `${sessionId}\0${runId}`;
}

function peerTaskHandlerKey(targetId: string, targetRunId: string): string {
  return terminalHandlerKey(targetId, targetRunId);
}

function retainPeerTask(event: PeerTaskEventPayload): void {
  const targetKey = peerTaskHandlerKey(event.target_id, event.target_run_id);
  let pending = pendingPeerTasks.get(targetKey);
  if (!pending) {
    while (pendingPeerTasks.size >= MAX_PENDING_PEER_TASK_TARGETS) {
      const oldestTarget = pendingPeerTasks.keys().next().value;
      if (!oldestTarget) break;
      pendingPeerTasks.delete(oldestTarget);
    }
    pending = new Map();
    pendingPeerTasks.set(targetKey, pending);
  }
  pending.set(event.job_id, event);
  while (pending.size > MAX_PENDING_PEER_TASKS_PER_TARGET) {
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
      const handler = outputHandlers.get(terminalHandlerKey(event.payload.id, event.payload.run_id));
      if (handler) handler(event.payload.data);
    });
    const unStatus = await listen<StatusEventPayload>('tier-terminal-status', (event) => {
      const handler = statusHandlers.get(terminalHandlerKey(event.payload.id, event.payload.run_id));
      if (handler) handler(event.payload.running, event.payload.exit_code);
    });
    const unCwd = await listen<CwdEventPayload>('tier-terminal-cwd', (event) => {
      const handler = cwdHandlers.get(terminalHandlerKey(event.payload.id, event.payload.run_id));
      if (handler) handler(event.payload.cwd);
    });
    const unExit = await listen<ExitEventPayload>('tier-terminal-exit', (event) => {
      const handler = exitHandlers.get(terminalHandlerKey(event.payload.id, event.payload.run_id));
      if (handler) handler(event.payload.exit_code);
    });
    const unPeerTask = await listen<PeerTaskEventPayload>('multi-agent-task-complete', (event) => {
      const handler = peerTaskHandlers.get(
        peerTaskHandlerKey(event.payload.target_id, event.payload.target_run_id),
      );
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
 * Subscribe to PTY events for a specific terminal run.
 * Returns an unsubscribe function. Safe to call before or after the global
 * Tauri listeners are initialized — initialization is lazy and shared.
 *
 * Only one handler per (session, run, event type) is supported. Calling
 * subscribe again for the same run overwrites its previous handlers.
 */
export async function subscribeTerminalEvents(
  sessionId: string,
  runId: string,
  handlers: TerminalEventHandlers,
  isCurrent?: () => boolean,
): Promise<() => void> {
  await ensureInit();
  // A React cleanup can run while the shared listener initialization is still
  // pending. Do not let that stale async continuation overwrite a newer run's
  // peer-task handler after it finally resumes.
  if (isCurrent && !isCurrent()) return () => {};
  const terminalKey = terminalHandlerKey(sessionId, runId);

  // Capture references to the handlers we just registered.
  // The unsub function must only remove OUR handlers, not a newer mount's.
  const myOutput = handlers.onOutput;
  const myStatus = handlers.onStatus;
  const myCwd = handlers.onCwd;
  const myExit = handlers.onExit;
  const myPeerTask = handlers.onPeerTask;
  const peerTaskKey = peerTaskHandlerKey(sessionId, runId);

  if (myOutput) outputHandlers.set(terminalKey, myOutput);
  if (myStatus) statusHandlers.set(terminalKey, myStatus);
  if (myCwd) cwdHandlers.set(terminalKey, myCwd);
  if (myExit) exitHandlers.set(terminalKey, myExit);
  if (myPeerTask) {
    peerTaskHandlers.set(peerTaskKey, myPeerTask);
    const pending = pendingPeerTasks.get(peerTaskKey);
    if (pending) {
      pendingPeerTasks.delete(peerTaskKey);
      for (const event of pending.values()) myPeerTask(event);
    }
  }

  return () => {
    // Only delete if the registered handler is still ours.
    // React Strict Mode double-mounts components. The same terminal run can
    // be subscribed more than once while effects settle, so only remove a
    // handler if this cleanup still owns that exact map entry.
    if (myOutput && outputHandlers.get(terminalKey) === myOutput) outputHandlers.delete(terminalKey);
    if (myStatus && statusHandlers.get(terminalKey) === myStatus) statusHandlers.delete(terminalKey);
    if (myCwd && cwdHandlers.get(terminalKey) === myCwd) cwdHandlers.delete(terminalKey);
    if (myExit && exitHandlers.get(terminalKey) === myExit) exitHandlers.delete(terminalKey);
    if (myPeerTask && peerTaskHandlers.get(peerTaskKey) === myPeerTask) peerTaskHandlers.delete(peerTaskKey);
  };
}
