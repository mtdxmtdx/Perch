// 常见服务商的预设（设置页"添加模型"和新用户引导共用）。模型名只是示例，以厂商文档为准。

import type { Provider } from "./api";

export interface Preset {
  label: string;
  base_url: string;
  model: string;
  style: string;
  vision?: boolean;
  /** 向量模型（/embeddings），不能用来聊天 */
  embedding?: boolean;
  /** 拿 API Key 的页面 */
  keyUrl?: string;
  /** 新用户引导里的一句说明 */
  hint?: string;
}

export const PRESETS: Preset[] = [
  {
    label: "DeepSeek",
    base_url: "https://api.deepseek.com/v1",
    model: "deepseek-flash",
    style: "deepseek",
    vision: true,
    keyUrl: "https://platform.deepseek.com/api_keys",
    hint: "国内直连，便宜好用，推荐",
  },
  {
    label: "通义千问（阿里云百炼）",
    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    model: "qwen-plus",
    style: "qwen",
    keyUrl: "https://bailian.console.aliyun.com/",
    hint: "国内直连，新用户有免费额度",
  },
  {
    label: "硅基流动",
    base_url: "https://api.siliconflow.cn/v1",
    model: "deepseek-ai/DeepSeek-V3",
    style: "openai",
    keyUrl: "https://cloud.siliconflow.cn/account/ak",
    hint: "国内直连，模型多，也提供向量模型",
  },
  {
    label: "OpenRouter",
    base_url: "https://openrouter.ai/api/v1",
    model: "openai/gpt-4o-mini",
    style: "openrouter",
    keyUrl: "https://openrouter.ai/settings/keys",
    hint: "一个 Key 用各家模型，国内可能需要代理",
  },
  {
    label: "OpenAI",
    base_url: "https://api.openai.com/v1",
    model: "gpt-4o-mini",
    style: "openai",
    keyUrl: "https://platform.openai.com/api-keys",
    hint: "国内需要代理",
  },
  { label: "向量模型：硅基流动 bge-m3", base_url: "https://api.siliconflow.cn/v1", model: "BAAI/bge-m3", style: "openai", embedding: true },
  { label: "向量模型：通义 text-embedding-v4", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "text-embedding-v4", style: "openai", embedding: true },
  { label: "向量模型：OpenAI text-embedding-3-small", base_url: "https://api.openai.com/v1", model: "text-embedding-3-small", style: "openai", embedding: true },
  { label: "自定义", base_url: "https://", model: "", style: "openai" },
];

/** 新用户引导里能选的：能聊天、地址和模型名都填好了的 */
export const CHAT_PRESETS = PRESETS.filter((p) => !p.embedding && p.model);

export function providerFrom(p: Preset, id: string): Provider {
  return {
    id,
    name: p.label,
    base_url: p.base_url,
    model: p.model,
    supports_vision: !!p.vision,
    supports_tools: true,
    reasoning_effort: "",
    reasoning_style: p.style,
  };
}
