use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 持久化在 %APPDATA%\Perch\config.json 的用户设置。API Key 不在这里，存在 Windows 凭据管理器。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 当前皮肤：皮肤目录的绝对路径，或内置皮肤的 id；为空时用内置的栖栖
    pub skin: Option<String>,
    /// 已做过"内置栖栖"上线时的迁移（老用户保留原来看到的皮肤）
    pub builtin_skin_migrated: bool,
    /// 已经问过要不要接对话模型（新用户引导），回答过就不再主动问
    pub model_onboarded: bool,
    /// 相对一格精灵（Codex 为 192×208）的缩放比例
    pub scale: f64,
    pub work_mode: bool,
    pub hide_on_fullscreen: bool,
    /// 只在这块显示器上活动（设备名，比如 \\.\DISPLAY1）；为空表示所有显示器
    pub pet_screen: Option<String>,
    /// 所在显示器有全屏程序时换到别的显示器，而不是隐藏
    pub fullscreen_switch: bool,
    /// 上次在哪块屏、横向位置，下次启动从这里落下
    pub pet_place: Option<PetPlace>,
    /// 时区名（比如 Asia/Tokyo）；为空跟随系统（见 clock.rs）
    pub timezone: Option<String>,
    pub providers: Vec<Provider>,
    pub roles: Roles,
    pub personas: Vec<Persona>,
    pub active_persona: String,
    pub chat: ChatParams,
    pub tools: ToolParams,
    pub image: ImageParams,
    pub voice: VoiceParams,
    pub agents: AgentsParams,
    /// 【留接口】TTS 语音输出（见 tts.rs）
    pub tts: TtsParams,
    /// 主动聊天、提醒（见 proactive.rs）
    pub proactive: ProactiveParams,
    /// 桌面感知：按需看屏等（见 awareness/）
    pub awareness: AwarenessParams,
}

/// 桌宠上次待的地方
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PetPlace {
    /// 显示器设备名
    pub screen: String,
    /// 在工作区里的横向位置，0 左边 ~ 1 右边
    pub x: f64,
}

/// 接入 Claude Code / Codex / DeepSeek Harness，汇报它们的工作状态
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsParams {
    /// 本地事件服务的端口（只监听 127.0.0.1）
    pub port: u16,
    pub claude_code: bool,
    pub codex: bool,
    pub dsh: bool,
    /// 任务完成时把最后的回复交给辅助模型，用人格口吻总结；关掉则只用模板话术，不转发任何内容
    pub summarize: bool,
    /// 完成、需要授权、出错时发 Windows 通知
    pub notify: bool,
    /// 一轮至少跑了这么多秒才汇报"完成"（对话式使用时每句回复都算一轮，太短的不打扰）
    pub min_report_seconds: u64,
    /// 在桌宠身边显示状态标签（"Claude Code · 思考中"）
    pub status_tag: bool,
    /// 已经问过要不要接入本机检测到的 agent（只在第一次启动问一次）
    pub onboarded: bool,
}

impl Default for AgentsParams {
    fn default() -> Self {
        Self { port: 47821, claude_code: false, codex: false, dsh: false, summarize: true, notify: true, min_report_seconds: 20, status_tag: true, onboarded: false }
    }
}

/// 【留接口】TTS 语音输出。还没有引擎和设置界面，默认关闭
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsParams {
    pub enabled: bool,
    /// 朗读引擎："none" 不读 / "cloud" OpenAI 兼容的 /audio/speech / "local" GPT-SoVITS 风格的本地服务
    pub provider: String,
    /// 云端的音色（比如 alloy、FunAudioLLM/CosyVoice2-0.5B:anna）；为空用服务默认的
    pub voice: Option<String>,
    /// 语速倍数
    pub speed: f64,
    /// 音量倍数
    pub volume: f64,
    /// 读对话回复（只读开头）
    pub read_replies: bool,
    /// 对话回复最多读多少字（按句子截）
    pub reply_chars: usize,
    /// 读 agent 汇报和主动聊天说的话
    pub read_reports: bool,
    /// 工作模式、免打扰时段不出声（全屏、开会、直播时总是不出声）
    pub mute_when_quiet: bool,
    /// 本地服务地址
    pub local_url: String,
    /// 参考音频的路径（本地服务那台机器上的路径）
    pub local_ref_audio: Option<String>,
    /// 参考音频里说的话（可选，填了更像）
    pub local_ref_text: Option<String>,
    pub local_ref_lang: String,
    /// 要读的文字的语言
    pub local_text_lang: String,
    /// 朗读语言（zh、ja、en……）：朗读前翻译成这种语言，显示的文字不变；为空读原文
    pub speak_language: Option<String>,
    /// Perch 启动时运行本地服务的启动脚本，退出时关掉它
    pub local_autostart: bool,
    /// 启动脚本（.bat、.cmd 或 .exe）
    pub local_start_script: Option<String>,
    /// 服务里预置的角色声音（Qwen3TTS 的 LoRA）；选了就不用参考音频
    pub local_lora: Option<String>,
    pub local_speaker: Option<String>,
    /// 逐句合成：第一句合成好就开始读，对话回复还在生成时也开始读
    pub split_sentences: bool,
}

impl Default for TtsParams {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "none".into(),
            voice: None,
            speed: 1.0,
            volume: 1.0,
            read_replies: true,
            reply_chars: 60,
            read_reports: true,
            mute_when_quiet: true,
            local_url: "http://127.0.0.1:9880/".into(),
            local_ref_audio: None,
            local_ref_text: None,
            local_ref_lang: "zh".into(),
            local_text_lang: "zh".into(),
            speak_language: None,
            local_autostart: false,
            local_start_script: None,
            local_lora: None,
            local_speaker: None,
            split_sentences: true,
        }
    }
}


/// 桌面感知（按需看屏）。锁死名单是代码常量，不在这里。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AwarenessParams {
    /// 是否提供 see_screen 工具（需同时已授权）
    pub vision_tool: bool,
    /// 设置里勾选「允许看屏」
    pub vision_authorized: bool,
    /// 截图长边像素；0 表示不缩放
    pub see_screen_long_side: u32,
    /// 开启后只截白名单里的进程（锁死名单仍优先）
    pub capture_whitelist_mode: bool,
    /// 白名单进程名（不带 .exe）
    pub capture_whitelist: Vec<String>,
    /// 用户拦截：进程名或标题关键字（预置聊天应用与无痕关键字，可删可加）
    pub capture_blocklist_user: Vec<String>,
    /// 主动聊天写话时附带前台程序名+标题（最多 140 字）；被门禁拦住的不给
    pub include_title: bool,
    /// 主动看屏（screen-glance 闲聊触发器）
    pub proactive_vision: bool,
    /// 两次主动看屏最短间隔（分钟）
    pub vision_min_interval_minutes: u64,
    /// 每天最多主动看屏次数
    pub vision_daily_cap: u32,
    /// 进程/标题规则：命中则看一眼
    pub vision_process_rules: Vec<VisionProcessRule>,
    /// 前台窗口或标题相对上次看屏变化时，用 cuts 比对是否显著
    pub vision_on_significant_change: bool,
}

/// 主动看屏：进程名（不带 .exe）与可选的本地标题关键字
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VisionProcessRule {
    pub process: String,
    #[serde(default)]
    pub title_keyword: Option<String>,
}

impl Default for VisionProcessRule {
    fn default() -> Self {
        Self { process: String::new(), title_keyword: None }
    }
}

impl Default for AwarenessParams {
    fn default() -> Self {
        let mut block = crate::awareness::gate::default_blocklist_processes();
        block.extend(crate::awareness::gate::default_blocklist_title_keywords());
        Self {
            vision_tool: false,
            vision_authorized: false,
            see_screen_long_side: 1280,
            capture_whitelist_mode: false,
            capture_whitelist: Vec::new(),
            capture_blocklist_user: block,
            include_title: false,
            proactive_vision: false,
            vision_min_interval_minutes: 10,
            vision_daily_cap: 30,
            vision_process_rules: Vec::new(),
            vision_on_significant_change: true,
        }
    }
}

/// 主动聊天、提醒（见 proactive.rs），默认关闭
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProactiveParams {
    pub enabled: bool,
    /// 用辅助模型按人格口吻写要说的话；关掉只用模板话术（不发给云端）
    pub use_model: bool,
    /// 免打扰时段 "HH:MM"，可以跨午夜；期间只说重要的提醒。两个都为空表示不设
    pub quiet_start: Option<String>,
    pub quiet_end: Option<String>,
    /// 两次主动闲聊至少隔多少分钟（重要的提醒不受限制）
    pub min_interval_minutes: u64,
    /// 用户在电脑前、但这么多分钟没聊天时找你聊两句；为空不启用
    pub no_chat_minutes: Option<u64>,
    /// 连续用电脑这么多分钟没休息时提醒活动；为空不启用
    pub sedentary_minutes: Option<u64>,
    /// 就寝时间 "HH:MM"：到点后还在用电脑就催睡；为空不启用
    pub bedtime: Option<String>,
    /// 每天定时提醒
    pub daily: Vec<DailyReminder>,
    /// agent 等你授权超过 5 分钟还没处理时，再提一次
    pub agent_follow_up: bool,
    /// 按前台程序切换模式：写代码时只说重要的和鼓励，全屏、开会、直播时什么都不说
    pub desktop_modes: bool,
    /// 算"专注"的程序（进程名，不带 .exe，不分大小写）；agent 在干活时也算专注
    pub focus_apps: Vec<String>,
    /// 算"静默"的程序（开会、直播录屏）；全屏程序总是算静默
    pub silent_apps: Vec<String>,
    /// 看视频（「在看」）时仍放行的重要触发器 id；闲聊 id 写进来也不放行
    pub media_allowed_triggers: Vec<String>,
    /// 全屏时仍放行的重要触发器 id（以系统通知送达）；闲聊 id 写进来也不放行
    pub fullscreen_allowed_triggers: Vec<String>,
    /// 进入「在看」：`user_idle_secs` 下限（秒）
    pub watching_idle_secs: u64,
    /// 退出「在看」滞回：前台持续无声秒数
    pub watching_hold_secs: u64,
    /// 专注这么多分钟后说句鼓励；为空不启用
    pub encourage_minutes: Option<u64>,
    /// 离开电脑这么多分钟后回来时打招呼（每天早上第一次见面问早安）；为空不启用
    pub welcome_back_minutes: Option<u64>,
    /// 节日问候（元旦、春节、中秋等公历和农历节日）
    pub holidays: bool,
    /// 用户生日 "MM-DD"，当天送上祝福；为空不设
    pub birthday: Option<String>,
    /// 纪念日，当天提一句
    pub anniversaries: Vec<Anniversary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Anniversary {
    /// "MM-DD"
    pub date: String,
    pub label: String,
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

impl Default for ProactiveParams {
    fn default() -> Self {
        Self {
            enabled: false,
            use_model: true,
            quiet_start: Some("23:30".into()),
            quiet_end: Some("07:30".into()),
            min_interval_minutes: 60,
            no_chat_minutes: Some(120),
            sedentary_minutes: Some(60),
            bedtime: None,
            daily: Vec::new(),
            agent_follow_up: true,
            desktop_modes: true,
            focus_apps: names(&[
                "code", "cursor", "windsurf", "trae", "zed", "devenv", "idea64", "pycharm64", "webstorm64", "clion64",
                "goland64", "rider64", "rustrover64", "studio64", "sublime_text", "notepad++", "windowsterminal",
                "nvim-qt",
            ]),
            silent_apps: names(&["ms-teams", "teams", "zoom", "wemeetapp", "voovmeetingapp", "obs64", "streamlabs obs"]),
            media_allowed_triggers: names(&["sedentary", "daily-at"]),
            fullscreen_allowed_triggers: names(&["daily-at"]),
            watching_idle_secs: 45,
            watching_hold_secs: 30,
            encourage_minutes: Some(90),
            welcome_back_minutes: Some(60),
            holidays: true,
            birthday: None,
            anniversaries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DailyReminder {
    /// "HH:MM"，24 小时制
    pub at: String,
    pub label: String,
}

/// 语音输入
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceParams {
    /// 开启语音输入（默认关闭：要先下载语音组件，或者指定已有的运行库和模型）
    pub enabled: bool,
    /// 按住说话的全局快捷键，例如 Ctrl+Shift+Space；为空表示不用快捷键
    pub hotkey: String,
    /// 识别完直接发送；false 时只填进输入框
    pub auto_send: bool,
    /// 麦克风名称；为空用系统默认
    pub device: Option<String>,
    /// 手动指定的 SenseVoice 模型文件夹；为空用自动下载的
    pub model_dir: Option<String>,
    /// 手动指定的语音运行库文件夹（里面有 sherpa-onnx-c-api.dll 和 onnxruntime.dll）；为空用下载的
    pub runtime_dir: Option<String>,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self { enabled: false, hotkey: "Ctrl+Shift+Space".into(), auto_send: true, device: None, model_dir: None, runtime_dir: None }
    }
}

/// 内置文件工具（read_file / write_file / edit_file）
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolParams {
    pub enabled: bool,
    /// 允许访问的目录（白名单），之外的路径一律拒绝
    pub allowed_dirs: Vec<String>,
    /// 一轮（用户一条消息引发的）最多调用几次工具
    pub max_calls: usize,
}

impl Default for ToolParams {
    fn default() -> Self {
        Self { enabled: true, allowed_dirs: Vec::new(), max_calls: 20 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageParams {
    /// 每天最多生成几张
    pub daily_limit: u32,
    /// 传给生图接口的 size，例如 1024x1024
    pub size: String,
}

impl Default for ImageParams {
    fn default() -> Self {
        Self { daily_limit: 20, size: "1024x1024".into() }
    }
}

/// 一个 OpenAI 兼容的模型接入点
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Provider {
    pub id: String,
    pub name: String,
    /// 例如 https://api.deepseek.com/v1（不含 /chat/completions）
    pub base_url: String,
    pub model: String,
    pub supports_vision: bool,
    pub supports_tools: bool,
    /// 推理强度：""（不设置，不发参数）/ off / low / medium / high
    pub reasoning_effort: String,
    /// 推理参数的格式：openai（reasoning_effort）/ openrouter（reasoning.effort）/ qwen（enable_thinking）/ deepseek（thinking + reasoning_effort）
    pub reasoning_style: String,
}

impl Provider {
    /// 记录"这个模型不接受推理参数"的 key；改了模型名或参数格式就会重新尝试
    pub fn reasoning_flag_key(&self) -> String {
        format!("reasoning_unsupported:{}:{}:{}", self.id, self.model, self.reasoning_style)
    }
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            base_url: String::new(),
            model: String::new(),
            supports_vision: false,
            supports_tools: true,
            reasoning_effort: String::new(),
            reasoning_style: "openai".into(),
        }
    }
}

/// 各用途使用哪个 provider（填 provider id）
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Roles {
    pub chat: Option<String>,
    /// 主模型不能看图时，先用它把图片转成文字
    pub vision: Option<String>,
    /// 提取记忆、滚动总结、改写汇报；为空时用主模型
    pub aux: Option<String>,
    /// 生图（M3）
    pub image: Option<String>,
    /// 向量模型（OpenAI 兼容的 /embeddings）：配了就按相关度挑事件记忆，不配就全部注入
    pub embedding: Option<String>,
    /// 语音合成（OpenAI 兼容的 /audio/speech），朗读引擎选"云端"时用
    pub tts: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DialogPair {
    pub user: String,
    pub assistant: String,
}

impl Default for DialogPair {
    fn default() -> Self {
        Self { user: String::new(), assistant: String::new() }
    }
}

/// 人格，字段参考 AstrBot
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Persona {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    /// 预设对话，拼在历史消息前面，用来定口吻
    pub begin_dialogs: Vec<DialogPair>,
    /// 覆盖主对话用的 provider
    pub provider: Option<String>,
    pub temperature: Option<f64>,
    /// 绑定的皮肤目录；切换到这个人格时自动换皮肤
    pub skin: Option<String>,
    /// 这个人格自己的声音，没填的项跟随「语音」里的设置
    pub voice: PersonaVoice,
}

/// 人格自己的声音。每一项都是可选的：填了就覆盖「语音」页的设置，没填就跟随
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaVoice {
    /// 云端音色
    pub voice: Option<String>,
    /// 本地服务里的角色声音
    pub local_lora: Option<String>,
    pub local_speaker: Option<String>,
    /// 本地服务的参考音频（选了就不用角色声音）
    pub local_ref_audio: Option<String>,
    pub local_ref_text: Option<String>,
    pub local_ref_lang: Option<String>,
    /// 朗读语言：不填跟随；填空字符串表示不翻译
    pub speak_language: Option<String>,
}

fn filled(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

impl TtsParams {
    /// 套上人格自己的声音
    pub fn for_persona(&self, v: &PersonaVoice) -> TtsParams {
        let mut t = self.clone();
        if let Some(voice) = filled(&v.voice) {
            t.voice = Some(voice);
        }
        if let Some(audio) = filled(&v.local_ref_audio) {
            // 人格用参考音频：不用全局的角色声音
            t.local_lora = None;
            t.local_speaker = None;
            t.local_ref_audio = Some(audio);
            t.local_ref_text = filled(&v.local_ref_text);
            if let Some(lang) = filled(&v.local_ref_lang) {
                t.local_ref_lang = lang;
            }
        } else if let Some(lora) = filled(&v.local_lora) {
            t.local_lora = Some(lora);
            t.local_speaker = filled(&v.local_speaker);
        }
        if let Some(lang) = &v.speak_language {
            t.speak_language = filled(&Some(lang.clone()));
        }
        t
    }
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            id: "default".into(),
            name: "栖栖".into(),
            system_prompt: DEFAULT_PROMPT.into(),
            begin_dialogs: Vec::new(),
            provider: None,
            temperature: None,
            skin: None,
            voice: PersonaVoice::default(),
        }
    }
}

const DEFAULT_PROMPT: &str = "你是栖栖，一只停在用户电脑屏幕窗沿上的蓝白小鸟，也是用户的桌面伙伴。\
性格活泼、贴心，说话简短自然，像朋友聊天一样；偶尔流露一点小鸟的习惯（比如歪头、扑腾翅膀），但不要每句都提。\
回答尽量控制在三四句话以内，除非用户需要详细解释。不要使用夸张的颜文字堆砌。";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatParams {
    /// 历史消息的 token 预算（估算），超过就把最早的一段并入滚动总结
    pub context_tokens: usize,
    /// 每隔几轮用户消息提取一次长期记忆
    pub memory_extract_every: usize,
    /// 记忆条数上限，超过就合并去重
    pub memory_limit: usize,
    /// 配了向量模型时的记忆条数上限（只挑相关的放进提示词，可以记得更多）
    pub memory_limit_retrieval: usize,
    /// 配了向量模型时，每次放进提示词的相关事件记忆条数（另外总会带上最近的几条）
    pub memory_top_k: usize,
    /// 单个文件附件最多注入多少字
    pub file_chars: usize,
    /// 气泡里最多显示多少字，超出提示展开
    pub bubble_chars: usize,
    /// 关系数值（好感、信任、熟悉、心情）：影响回复语气，由辅助模型随对话调整
    pub relationship: bool,
}

impl Default for ChatParams {
    fn default() -> Self {
        Self {
            context_tokens: 24000,
            memory_extract_every: 5,
            memory_limit: 80,
            memory_limit_retrieval: 400,
            memory_top_k: 8,
            file_chars: 20000,
            bubble_chars: 160,
            relationship: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            skin: None,
            builtin_skin_migrated: false,
            model_onboarded: false,
            scale: 0.75,
            work_mode: false,
            hide_on_fullscreen: true,
            pet_screen: None,
            fullscreen_switch: false,
            pet_place: None,
            timezone: None,
            providers: Vec::new(),
            roles: Roles::default(),
            personas: vec![Persona::default()],
            active_persona: "default".into(),
            chat: ChatParams::default(),
            tools: ToolParams::default(),
            image: ImageParams::default(),
            voice: VoiceParams::default(),
            tts: TtsParams::default(),
            proactive: ProactiveParams::default(),
            awareness: AwarenessParams::default(),
            agents: AgentsParams::default(),
        }
    }
}

impl Config {
    pub fn provider(&self, id: Option<&str>) -> Option<&Provider> {
        let id = id?;
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn active_persona(&self) -> Persona {
        self.personas
            .iter()
            .find(|p| p.id == self.active_persona)
            .or(self.personas.first())
            .cloned()
            .unwrap_or_default()
    }

    /// 主对话 provider：人格覆盖 > 用途配置
    pub fn chat_provider(&self, persona: &Persona) -> Option<&Provider> {
        self.provider(persona.provider.as_deref())
            .or_else(|| self.provider(self.roles.chat.as_deref()))
    }

    pub fn aux_provider(&self, persona: &Persona) -> Option<&Provider> {
        self.provider(self.roles.aux.as_deref())
            .or_else(|| self.chat_provider(persona))
    }

    /// 保证至少有一个人格，且 active_persona 有效
    pub fn normalize(&mut self) {
        if self.personas.is_empty() {
            self.personas.push(Persona::default());
        }
        if !self.personas.iter().any(|p| p.id == self.active_persona) {
            self.active_persona = self.personas[0].id.clone();
        }
    }
}

/// 数据目录名。改名前叫 table-pet（`%APPDATA%\table-pet`），启动时搬到新目录
const APP_DIR: &str = "Perch";
const LEGACY_APP_DIR: &str = "table-pet";

/// 测试时用环境变量指定的数据目录（避免碰到真实数据）；也认改名前的变量名
fn data_dir_override() -> Option<PathBuf> {
    std::env::var_os("PERCH_DATA_DIR").or_else(|| std::env::var_os("TABLE_PET_DATA_DIR")).map(PathBuf::from)
}

/// 指定了测试数据目录
pub fn data_dir_overridden() -> bool {
    data_dir_override().is_some()
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = data_dir_override() {
        return dir;
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let (dir, legacy) = (base.join(APP_DIR), base.join(LEGACY_APP_DIR));
    // 旧目录没能搬走（比如文件被占用）时这次继续用旧目录，不丢数据，下次启动再搬
    if !dir.exists() && legacy.exists() {
        legacy
    } else {
        dir
    }
}

/// 这次启动刚把改名前的数据搬过来（之后要顺手更新各 agent 的接入）
pub static JUST_MIGRATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 把改名前的数据目录 %APPDATA%\table-pet 整个搬到 %APPDATA%\Perch（同一个盘，直接改名）。
/// 新目录已存在就不动；搬失败就留在原处继续用。要在读配置、开数据库之前调用。
pub fn migrate_legacy_dir() {
    if data_dir_overridden() {
        return;
    }
    let Some(base) = dirs::config_dir() else { return };
    match migrate_dir_in(&base) {
        Ok(true) => JUST_MIGRATED.store(true, std::sync::atomic::Ordering::SeqCst),
        Ok(false) => {}
        Err(e) => eprintln!("[migrate] 搬迁数据目录失败，这次继续用旧目录：{e}"),
    }
}

/// 在 base 下把旧目录改名成新目录；返回是否搬了
fn migrate_dir_in(base: &std::path::Path) -> std::io::Result<bool> {
    let (dir, legacy) = (base.join(APP_DIR), base.join(LEGACY_APP_DIR));
    if dir.exists() || !legacy.exists() {
        return Ok(false);
    }
    std::fs::rename(&legacy, &dir).map(|_| true)
}

#[cfg(test)]
mod migrate_tests {
    use super::*;

    #[test]
    fn moves_legacy_dir_once_and_keeps_contents() {
        let base = std::env::temp_dir().join(format!("perch-mig-{}", uuid::Uuid::new_v4()));
        // 没有旧目录：什么都不做
        std::fs::create_dir_all(&base).unwrap();
        assert!(!migrate_dir_in(&base).unwrap());
        // 有旧目录：整个搬过去，内容不变
        std::fs::create_dir_all(base.join(LEGACY_APP_DIR).join("attachments")).unwrap();
        std::fs::write(base.join(LEGACY_APP_DIR).join("config.json"), "{\"a\":1}").unwrap();
        std::fs::write(base.join(LEGACY_APP_DIR).join("attachments").join("x.png"), b"png").unwrap();
        assert!(migrate_dir_in(&base).unwrap());
        assert!(!base.join(LEGACY_APP_DIR).exists());
        assert_eq!(std::fs::read_to_string(base.join(APP_DIR).join("config.json")).unwrap(), "{\"a\":1}");
        assert_eq!(std::fs::read(base.join(APP_DIR).join("attachments").join("x.png")).unwrap(), b"png");
        // 新目录已存在：旧目录（比如又被老版本建出来）不会覆盖新数据
        std::fs::create_dir_all(base.join(LEGACY_APP_DIR)).unwrap();
        assert!(!migrate_dir_in(&base).unwrap());
        assert!(base.join(LEGACY_APP_DIR).exists());
        let _ = std::fs::remove_dir_all(base);
    }

    /// 用一个随机 id 在真实凭据管理器里走一遍旧名 → 新名，结束后清掉
    #[test]
    fn moves_keys_to_new_service() {
        let id = format!("perch-test-{}", uuid::Uuid::new_v4());
        keyring::Entry::new(LEGACY_KEYRING_SERVICE, &id).unwrap().set_password("sk-old").unwrap();
        migrate_legacy_keys(&[id.clone()]);
        assert_eq!(get_api_key(&id).as_deref(), Some("sk-old"));
        assert!(keyring::Entry::new(LEGACY_KEYRING_SERVICE, &id).unwrap().get_password().is_err());
        // 新名下已有 Key 时不覆盖
        keyring::Entry::new(LEGACY_KEYRING_SERVICE, &id).unwrap().set_password("sk-older").unwrap();
        migrate_legacy_keys(&[id.clone()]);
        assert_eq!(get_api_key(&id).as_deref(), Some("sk-old"));
        set_api_key(&id, "").unwrap();
    }
}

#[cfg(test)]
mod awareness_serde_tests {
    use super::*;

    /// 老配置缺少 M2/M3 字段时，serde(default) 填入安全默认（全关、标题不上传）
    #[test]
    fn old_japanese_switch_becomes_speak_language() {
        let mut c = Config::default();
        migrate_speak_language(&mut c, Some(&serde_json::json!({ "tts": { "speak_japanese": true } })));
        assert_eq!(c.tts.speak_language.as_deref(), Some("ja"));

        let mut c = Config::default();
        migrate_speak_language(&mut c, Some(&serde_json::json!({ "tts": { "speak_japanese": false } })));
        assert!(c.tts.speak_language.is_none());

        // 已经选过语言的不动
        let mut c = Config::default();
        c.tts.speak_language = Some("en".into());
        migrate_speak_language(&mut c, Some(&serde_json::json!({ "tts": { "speak_japanese": true } })));
        assert_eq!(c.tts.speak_language.as_deref(), Some("en"));
    }

    #[test]
    fn awareness_missing_fields_get_safe_defaults() {
        let v: AwarenessParams = serde_json::from_str("{}").unwrap();
        assert!(!v.vision_tool);
        assert!(!v.vision_authorized);
        assert!(!v.include_title);
        assert!(!v.proactive_vision);
        assert_eq!(v.vision_min_interval_minutes, 10);
        assert_eq!(v.vision_daily_cap, 30);
        assert!(v.vision_on_significant_change);
        assert!(v.vision_process_rules.is_empty());
    }

    #[test]
    fn proactive_media_defaults_match_decision() {
        let p = ProactiveParams::default();
        // 已定决策：看视频 = 久坐 + 定时提醒；就寝可在设置里勾
        assert_eq!(p.media_allowed_triggers, vec!["sedentary".to_string(), "daily-at".to_string()]);
        assert!(p.fullscreen_allowed_triggers.iter().any(|s| s == "daily-at"));
        assert!(!p.fullscreen_allowed_triggers.iter().any(|s| s == "long-no-chat"));
    }

    #[test]
    fn config_roundtrip_keeps_m3_fields() {
        let mut c = Config::default();
        c.awareness.include_title = true;
        c.awareness.proactive_vision = true;
        c.awareness.vision_process_rules = vec![VisionProcessRule {
            process: "code".into(),
            title_keyword: Some("main".into()),
        }];
        let s = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&s).unwrap();
        assert!(back.awareness.include_title);
        assert!(back.awareness.proactive_vision);
        assert_eq!(back.awareness.vision_process_rules[0].process, "code");
    }
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// v0.5.0 只有「用日语说」开关，后来换成了朗读语言
fn migrate_speak_language(cfg: &mut Config, raw: Option<&serde_json::Value>) {
    let old = raw.and_then(|v| v.pointer("/tts/speak_japanese")).and_then(serde_json::Value::as_bool);
    if old == Some(true) && cfg.tts.speak_language.is_none() {
        cfg.tts.speak_language = Some("ja".into());
    }
}

pub fn load() -> Config {
    let raw: Option<serde_json::Value> = std::fs::read_to_string(config_path()).ok().and_then(|s| serde_json::from_str(&s).ok());
    let mut cfg: Config = raw.clone().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    migrate_speak_language(&mut cfg, raw.as_ref());
    // 语音输入开关是后来加的：老配置里没有这一项时，模型和运行库都在就保持开启，否则关闭
    if raw.as_ref().is_some_and(|v| v.pointer("/voice/enabled").is_none()) {
        cfg.voice.enabled = crate::voice::models::locate(cfg.voice.model_dir.as_deref()).is_ok()
            && crate::voice::runtime::locate(cfg.voice.runtime_dir.as_deref()).is_some();
    }
    // 内置栖栖上线前，没选过皮肤的用户看到的是自动挑的（uika 或第一个）：把它写进配置，升级后形象不变。新装的直接用栖栖
    if !cfg.builtin_skin_migrated {
        if raw.is_some() && cfg.skin.is_none() {
            cfg.skin = crate::skin::legacy_default().map(|e| e.id);
        }
        cfg.builtin_skin_migrated = true;
        if raw.is_some() {
            save(&cfg);
        }
    }
    cfg.normalize();
    cfg
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(cfg) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&path, s) {
                log::warn!("保存配置失败: {e}");
            }
        }
        Err(e) => log::warn!("序列化配置失败: {e}"),
    }
}

const KEYRING_SERVICE: &str = "perch";
/// 改名前存 API Key 用的凭据名
const LEGACY_KEYRING_SERVICE: &str = "table-pet";

/// 把改名前存在凭据管理器里的 API Key 挪到新名字下（新名字下已有的不覆盖），然后删掉旧的
pub fn migrate_legacy_keys(provider_ids: &[String]) {
    for id in provider_ids {
        let Ok(old) = keyring::Entry::new(LEGACY_KEYRING_SERVICE, id) else { continue };
        let Ok(key) = old.get_password() else { continue };
        if get_api_key(id).is_none() && set_api_key(id, &key).is_err() {
            continue;
        }
        let _ = old.delete_credential();
    }
}

pub fn get_api_key(provider_id: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, provider_id)
        .ok()?
        .get_password()
        .ok()
        .filter(|k| !k.is_empty())
}

pub fn set_api_key(provider_id: &str, key: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, provider_id).map_err(|e| e.to_string())?;
    if key.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        entry.set_password(key).map_err(|e| e.to_string())
    }
}
