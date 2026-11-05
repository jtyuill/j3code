//! Provider logos for the model picker, keyed by omp's provider id
//! (`anthropic`, `openai-codex`, `opencode-go`, …).
//!
//! Most logos come from LobeHub's icon set (MIT); the OpenCode, Xiaomi,
//! GitLab, NanoGPT and Synthetic marks come from OpenCode's provider icons
//! (MIT). Colored logos are drawn as images; monochrome ones as SVG masks
//! tinted with the text color, so they follow the theme.

use std::borrow::Cow;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, Pixels, Styled as _, div, img, svg,
};

/// Where logos live in the app's asset source.
const PREFIX: &str = "logos/";

struct Logo {
    /// `logos/<file>.svg`.
    path: &'static str,
    bytes: &'static [u8],
    /// Drawn in its own colors; otherwise tinted like text.
    colored: bool,
}

macro_rules! logos {
    ($($ident:ident = $file:literal $(, $colored:ident)?;)*) => {
        $(const $ident: Logo = Logo {
            path: concat!("logos/", $file, ".svg"),
            bytes: include_bytes!(concat!("../assets/logos/", $file, ".svg")),
            colored: logos!(@colored $($colored)?),
        };)*
        const ALL: &[&Logo] = &[$(&$ident),*];
    };
    (@colored colored) => { true };
    (@colored) => { false };
}

logos! {
    AIMLAPI = "aimlapi";
    ALIBABA = "alibaba-color", colored;
    ANTIGRAVITY = "antigravity-color", colored;
    APPLE = "apple";
    AZURE = "azure-color", colored;
    BAIDU = "baiducloud-color", colored;
    BASETEN = "baseten";
    BEDROCK = "bedrock-color", colored;
    CEREBRAS = "cerebras-color", colored;
    CLAUDE = "claude-color", colored;
    CLINE = "cline";
    CLOUDFLARE = "cloudflare-color", colored;
    COMMAND_CODE = "commandcode";
    CURSOR = "cursor";
    DEEPINFRA = "deepinfra-color", colored;
    DEEPSEEK = "deepseek-color", colored;
    DEVIN = "devin-color", colored;
    FIREWORKS = "fireworks-color", colored;
    GEMINI = "gemini-color", colored;
    GEMINI_CLI = "geminicli-color", colored;
    GITHUB_COPILOT = "githubcopilot";
    GITLAB = "gitlab";
    GMI = "gmicloud";
    GROQ = "groq";
    HUGGING_FACE = "huggingface-color", colored;
    INCEPTION = "inception";
    KILO = "kilocode";
    KIMI = "kimi-color", colored;
    META = "meta-color", colored;
    MINIMAX = "minimax-color", colored;
    MISTRAL = "mistral-color", colored;
    MOONSHOT = "moonshot";
    NANOGPT = "nanogpt";
    NOVITA = "novita-color", colored;
    NVIDIA = "nvidia-color", colored;
    OLLAMA = "ollama";
    OPENAI = "openai";
    OPENCODE = "opencode";
    OPENCODE_GO = "opencode-go";
    OPENROUTER = "openrouter";
    QWEN = "qwen-color", colored;
    SAKANA = "sakana-color", colored;
    SAMBANOVA = "sambanova-color", colored;
    SNOWFLAKE = "snowflake-color", colored;
    STEPFUN = "stepfun-color", colored;
    SYNTHETIC = "synthetic";
    TOGETHER = "together-color", colored;
    VENICE = "venice-color", colored;
    VERCEL = "vercel";
    XAI = "xai";
    XIAOMI = "xiaomi";
    ZAI = "zai";
    ZENMUX = "zenmux";
    ZHIPU = "zhipu-color", colored;
}

/// A provider's display name and logo.
fn lookup(provider: &str) -> Option<(&'static str, &'static Logo)> {
    Some(match provider {
        "aimlapi" => ("AI/ML API", &AIMLAPI),
        "alibaba-coding-plan" => ("Alibaba Coding Plan", &ALIBABA),
        "alibaba-token-plan" => ("Alibaba Token Plan", &ALIBABA),
        "amazon-bedrock" => ("Amazon Bedrock", &BEDROCK),
        "anthropic" => ("Anthropic", &CLAUDE),
        "apple" => ("Apple", &APPLE),
        "azure" => ("Azure", &AZURE),
        "baseten" => ("Baseten", &BASETEN),
        "bedrock-mantle" => ("Bedrock Mantle", &BEDROCK),
        "cerebras" => ("Cerebras", &CEREBRAS),
        "cline-pass" => ("Cline", &CLINE),
        "cloudflare-ai-gateway" => ("Cloudflare AI Gateway", &CLOUDFLARE),
        "cloudflare-workers-ai" => ("Cloudflare Workers AI", &CLOUDFLARE),
        "command-code" | "commandcode" => ("Command Code", &COMMAND_CODE),
        "cursor" => ("Cursor", &CURSOR),
        "deepinfra" => ("DeepInfra", &DEEPINFRA),
        "deepseek" => ("DeepSeek", &DEEPSEEK),
        "devin" => ("Devin", &DEVIN),
        "fireworks" => ("Fireworks", &FIREWORKS),
        "github-copilot" => ("GitHub Copilot", &GITHUB_COPILOT),
        "gitlab-duo" => ("GitLab Duo", &GITLAB),
        "gitlab-duo-agent" => ("GitLab Duo Agent", &GITLAB),
        "gmi-cloud" => ("GMI Cloud", &GMI),
        "google" => ("Google", &GEMINI),
        "google-antigravity" => ("Antigravity", &ANTIGRAVITY),
        "google-gemini-cli" => ("Gemini CLI", &GEMINI_CLI),
        "google-vertex" => ("Google Vertex", &GEMINI),
        "groq" => ("Groq", &GROQ),
        "huggingface" => ("Hugging Face", &HUGGING_FACE),
        "inception" => ("Inception", &INCEPTION),
        "kilo" => ("Kilo", &KILO),
        "kimi-code" => ("Kimi Code", &KIMI),
        "meta" => ("Meta", &META),
        "minimax" => ("MiniMax", &MINIMAX),
        "minimax-cn" => ("MiniMax (China)", &MINIMAX),
        "minimax-code" => ("MiniMax Code", &MINIMAX),
        "minimax-code-cn" => ("MiniMax Code (China)", &MINIMAX),
        "mistral" => ("Mistral", &MISTRAL),
        "moonshot" => ("Moonshot", &MOONSHOT),
        "nanogpt" => ("NanoGPT", &NANOGPT),
        "novita" => ("Novita", &NOVITA),
        "nvidia" => ("NVIDIA", &NVIDIA),
        "ollama" | "ollama-cloud" => ("Ollama", &OLLAMA),
        "openai" => ("OpenAI", &OPENAI),
        "openai-codex" => ("OpenAI Codex", &OPENAI),
        "opencode-go" => ("OpenCode Go", &OPENCODE_GO),
        "opencode-zen" => ("OpenCode Zen", &OPENCODE),
        "openrouter" => ("OpenRouter", &OPENROUTER),
        "qianfan" => ("Qianfan", &BAIDU),
        "qwen-portal" => ("Qwen", &QWEN),
        "sakana" => ("Sakana", &SAKANA),
        "sambanova" => ("SambaNova", &SAMBANOVA),
        "snowflake" => ("Snowflake", &SNOWFLAKE),
        "stepfun" => ("StepFun", &STEPFUN),
        "synthetic" => ("Synthetic", &SYNTHETIC),
        "together" => ("Together", &TOGETHER),
        "venice" => ("Venice", &VENICE),
        "vercel-ai-gateway" => ("Vercel AI Gateway", &VERCEL),
        "xai" | "xai-oauth" => ("xAI", &XAI),
        "xiaomi" => ("Xiaomi", &XIAOMI),
        "xiaomi-token-plan-ams" => ("Xiaomi (Amsterdam)", &XIAOMI),
        "xiaomi-token-plan-cn" => ("Xiaomi (China)", &XIAOMI),
        "xiaomi-token-plan-sgp" => ("Xiaomi (Singapore)", &XIAOMI),
        "zai" => ("Z.ai", &ZAI),
        "zenmux" => ("ZenMux", &ZENMUX),
        "zhipu-coding-plan" => ("Zhipu Coding Plan", &ZHIPU),
        _ => return None,
    })
}

/// The provider's display name; unknown ids show as they are.
pub fn provider_name(provider: &str) -> &str {
    lookup(provider).map_or(provider, |(name, _)| name)
}

/// The provider's logo at `size`; a generic glyph for providers without one.
pub fn provider_logo(provider: &str, size: Pixels, cx: &App) -> AnyElement {
    let Some((_, logo)) = lookup(provider) else {
        return Icon::new(Lucide::Bot)
            .size(size)
            .text_color(cx.theme().muted_foreground)
            .into_any_element();
    };
    if logo.colored {
        // Rasterized for the drawn size: `img(path)` would shrink a 2x
        // raster of the 64 px artwork on the GPU and alias.
        let key = logo.path.as_ptr() as u64;
        img(crate::svg_image::exact(
            key,
            logo.bytes,
            gpui_kit::size(size, size),
        ))
        .size(size)
        .flex_none()
        .into_any_element()
    } else {
        // `svg` paints the shape as a mask in the current text color.
        div()
            .flex_none()
            .size(size)
            .child(
                svg()
                    .path(logo.path)
                    .size(size)
                    .text_color(cx.theme().foreground),
            )
            .into_any_element()
    }
}

/// The logo files, for the app's asset source.
pub fn load(path: &str) -> Option<Cow<'static, [u8]>> {
    path.strip_prefix(PREFIX)?;
    ALL.iter()
        .find(|logo| logo.path == path)
        .map(|logo| Cow::Borrowed(logo.bytes))
}

/// The logo paths under `path`, for the app's asset source.
pub fn list(path: &str) -> impl Iterator<Item = &'static str> + '_ {
    ALL.iter()
        .map(|logo| logo.path)
        .filter(move |logo| logo.starts_with(path))
}
