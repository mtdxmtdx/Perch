// 与 Rust 后端 commands.rs 对应的类型和调用

import { invoke } from "@tauri-apps/api/core";

export interface Provider {
  id: string;
  name: string;
  base_url: string;
  model: string;
  supports_vision: boolean;
  supports_tools: boolean;
  /** "" 不设置 / off / low / medium / high */
  reasoning_effort: string;
  /** openai / openrouter / qwen */
  reasoning_style: string;
}

export interface ProviderView extends Provider {
  hasKey: boolean;
  /** 设置了推理强度但服务不接受，已自动忽略 */
  reasoningIgnored: boolean;
}

export interface Roles {
  chat: string | null;
  vision: string | null;
  aux: string | null;
  image: string | null;
}

export interface DialogPair {
  user: string;
  assistant: string;
}

export interface Persona {
  id: string;
  name: string;
  system_prompt: string;
  begin_dialogs: DialogPair[];
  provider: string | null;
  temperature: number | null;
  skin: string | null;
}

export interface ChatParams {
  context_tokens: number;
  memory_extract_every: number;
  memory_limit: number;
  file_chars: number;
  bubble_chars: number;
}

export interface ToolParams {
  enabled: boolean;
  allowed_dirs: string[];
  max_calls: number;
}

export interface ImageParams {
  daily_limit: number;
  size: string;
}

export interface VoiceParams {
  hotkey: string;
  auto_send: boolean;
  device: string | null;
  model_dir: string | null;
}

export interface VoiceStatus {
  modelReady: boolean;
  missing: string[];
  downloading: boolean;
  downloadDir: string;
  devices: string[];
  hotkeyError: string | null;
}

export interface DownloadProgress {
  file: string;
  downloaded: number;
  total: number;
  source: string;
}

export type VoiceEvent =
  | { state: "recording" }
  | { state: "level"; level: number }
  | { state: "partial"; text: string }
  | { state: "recognizing" }
  | { state: "idle" }
  | { state: "result"; text: string; sent: boolean }
  | { state: "error"; error: string };

export interface AgentsParams {
  port: number;
  claude_code: boolean;
  codex: boolean;
  dsh: boolean;
  summarize: boolean;
  notify: boolean;
  min_report_seconds: number;
  status_tag: boolean;
}

export type AgentId = "claude-code" | "codex" | "dsh";

/** 首次启动的接入询问里的一项 */
export interface OfferItem {
  agent: AgentId;
  name: string;
  ready: boolean;
  hint: string | null;
}

export interface OnboardResult {
  agent: AgentId;
  name: string;
  error: string | null;
}
export type HookState = "missing" | "ok" | "outdated";
export type SessionStatus = "idle" | "thinking" | "working" | "attention" | "done" | "error";

export interface AgentStatus {
  agent: AgentId;
  name: string;
  detected: boolean;
  enabled: boolean;
  state: HookState;
  note: string | null;
}

export interface SessionView {
  agent: AgentId;
  agentName: string;
  id: string;
  project: string;
  cwd: string;
  status: SessionStatus;
  tool: string | null;
  seconds: number;
  running: boolean;
}

export interface AgentsOverview {
  port: number | null;
  agents: AgentStatus[];
  sessions: SessionView[];
}

export interface SkinEntry {
  id: string;
  name: string;
}

export interface SettingsView {
  providers: ProviderView[];
  roles: Roles;
  personas: Persona[];
  activePersona: string;
  chat: ChatParams;
  tools: ToolParams;
  image: ImageParams;
  voice: VoiceParams;
  agents: AgentsParams;
  skins: SkinEntry[];
  picturesDir: string;
}

export interface AttachmentMeta {
  /** tool：本轮的工具操作记录（name 是标题，note 是状态，desc 是详情） */
  kind: "image" | "file" | "tool";
  name: string;
  file: string | null;
  desc: string | null;
  note: string | null;
}

export interface Message {
  id: number;
  persona: string;
  role: "user" | "assistant";
  display: string;
  attachments: AttachmentMeta[];
  createdAt: number;
}

export interface Memory {
  id: number;
  kind: "core" | "event";
  content: string;
  createdAt: number;
  updatedAt: number;
}

export type AttachmentIn =
  | { kind: "path"; path: string }
  | { kind: "imageData"; name: string; mime: string; data: string };

export type ChatEvent =
  | { type: "user"; persona: string; message: Message }
  | { type: "start"; persona: string }
  | { type: "delta"; persona: string; text: string }
  | { type: "done"; persona: string; message: Message }
  | { type: "error"; persona: string; error: string }
  | { type: "tool"; persona: string; id: string; title: string; status: ToolStatus; detail: string | null };

export type ToolStatus = "running" | "ok" | "error" | "denied";

export interface ToolLine {
  id: string;
  title: string;
  status: ToolStatus;
  detail: string | null;
}

export interface ApprovalRequest {
  id: number;
  tool: "write_file" | "edit_file";
  path: string;
  diff: string;
  added: number;
  removed: number;
  created: boolean;
}

export const api = {
  settingsGet: () => invoke<SettingsView>("settings_get"),
  settingsSave: (settings: {
    providers: Provider[];
    roles: Roles;
    personas: Persona[];
    activePersona: string;
    chat: ChatParams;
    tools: ToolParams;
    image: ImageParams;
    voice: VoiceParams;
    agents: AgentsParams;
  }) => invoke<void>("settings_save", { settings }),
  providerSetKey: (id: string, key: string) => invoke<void>("provider_set_key", { id, key }),
  providerTest: (provider: Provider, key?: string, imageOnly?: boolean) => invoke<string>("provider_test", { provider, key, imageOnly }),
  personaActivate: (id: string) => invoke<void>("persona_activate", { id }),
  chatHistory: (before?: number, limit?: number) =>
    invoke<{ persona: string; personaName: string; messages: Message[] }>("chat_history", { before, limit }),
  chatSend: (text: string, attachments: AttachmentIn[]) => invoke<void>("chat_send", { text, attachments }),
  chatCancel: () => invoke<void>("chat_cancel"),
  toolApprove: (id: number, decision: "allow" | "allowTurn" | "deny") => invoke<void>("tool_approve", { id, decision }),
  chatClear: (persona: string) => invoke<void>("chat_clear", { persona }),
  attachmentImage: (file: string) => invoke<ArrayBuffer>("attachment_image", { file }),
  memoryList: () => invoke<Memory[]>("memory_list"),
  memorySave: (id: number | null, kind: string, content: string) => invoke<void>("memory_save", { id, kind, content }),
  memoryDelete: (id: number) => invoke<void>("memory_delete", { id }),
  bubbleOpen: (mode: "input" | "speak") => invoke<void>("bubble_open", { mode }),
  bubbleClose: () => invoke<void>("bubble_close"),
  bubbleResize: (width: number, height: number) => invoke<void>("bubble_resize", { width, height }),
  statusResize: (width: number, height: number) => invoke<void>("status_resize", { width, height }),
  openChat: () => invoke<void>("open_chat"),
  voiceStatus: () => invoke<VoiceStatus>("voice_status"),
  agentsOverview: () => invoke<AgentsOverview>("agents_overview"),
  agentsSet: (agent: AgentId, enabled: boolean) => invoke<void>("agents_set", { agent, enabled }),
  agentsRepair: () => invoke<void>("agents_repair"),
  agentsOffer: () => invoke<OfferItem[] | null>("agents_offer"),
  agentsOnboard: (agents: AgentId[]) => invoke<OnboardResult[]>("agents_onboard", { agents }),
  voiceDownload: () => invoke<void>("voice_download"),
  voiceDownloadCancel: () => invoke<void>("voice_download_cancel"),
  voiceStart: () => invoke<void>("voice_start"),
  voiceStop: (send?: boolean) => invoke<string>("voice_stop", { send }),
  voiceCancel: () => invoke<void>("voice_cancel"),
  openSettings: () => invoke<void>("open_settings"),
};

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
