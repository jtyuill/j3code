//! Provider logos for the model picker and usage panel, keyed by omp's provider id
//! (`anthropic`, `openai-codex`, `opencode-go`, …), and the providers' display names.
//!
//! Most logos come from LobeHub's icon set (MIT); the OpenCode, Xiaomi, GitLab, NanoGPT and
//! Synthetic marks come from OpenCode's provider icons (MIT). They are `rune::Icon`s
//! generated into [`marks`]: coloured logos draw in their own colours, monochrome ones tint
//! with the element's colour so they follow the theme.

mod marks;

use rune::{El, Icon, icon};
use rune_kit::Kit;

use marks::*;

/// A provider's display name and logo.
fn lookup(provider: &str) -> Option<(&'static str, &'static Icon)> {
    Some(match provider {
        "aimlapi" => ("AI/ML API", &AIMLAPI),
        "alibaba-coding-plan" => ("Alibaba Coding Plan", &ALIBABA_COLOR),
        "alibaba-token-plan" => ("Alibaba Token Plan", &ALIBABA_COLOR),
        "amazon-bedrock" => ("Amazon Bedrock", &BEDROCK_COLOR),
        "anthropic" => ("Anthropic", &CLAUDE_COLOR),
        "apple" => ("Apple", &APPLE),
        "azure" => ("Azure", &AZURE_COLOR),
        "baseten" => ("Baseten", &BASETEN),
        "bedrock-mantle" => ("Bedrock Mantle", &BEDROCK_COLOR),
        "cerebras" => ("Cerebras", &CEREBRAS_COLOR),
        "cline-pass" => ("Cline", &CLINE),
        "cloudflare-ai-gateway" => ("Cloudflare AI Gateway", &CLOUDFLARE_COLOR),
        "cloudflare-workers-ai" => ("Cloudflare Workers AI", &CLOUDFLARE_COLOR),
        "command-code" | "commandcode" => ("Command Code", &COMMANDCODE),
        "cursor" => ("Cursor", &CURSOR),
        "deepinfra" => ("DeepInfra", &DEEPINFRA_COLOR),
        "deepseek" => ("DeepSeek", &DEEPSEEK_COLOR),
        "devin" => ("Devin", &DEVIN_COLOR),
        "fireworks" => ("Fireworks", &FIREWORKS_COLOR),
        "github-copilot" => ("GitHub Copilot", &GITHUBCOPILOT),
        "gitlab-duo" => ("GitLab Duo", &GITLAB),
        "gitlab-duo-agent" => ("GitLab Duo Agent", &GITLAB),
        "gmi-cloud" => ("GMI Cloud", &GMICLOUD),
        "google" => ("Google", &GEMINI_COLOR),
        "google-antigravity" => ("Antigravity", &ANTIGRAVITY_COLOR),
        "google-gemini-cli" => ("Gemini CLI", &GEMINICLI_COLOR),
        "google-vertex" => ("Google Vertex", &GEMINI_COLOR),
        "groq" => ("Groq", &GROQ),
        "huggingface" => ("Hugging Face", &HUGGINGFACE_COLOR),
        "inception" => ("Inception", &INCEPTION),
        "kilo" => ("Kilo", &KILOCODE),
        "kimi-code" => ("Kimi Code", &KIMI_COLOR),
        "meta" => ("Meta", &META_COLOR),
        "minimax" => ("MiniMax", &MINIMAX_COLOR),
        "minimax-cn" => ("MiniMax (China)", &MINIMAX_COLOR),
        "minimax-code" => ("MiniMax Code", &MINIMAX_COLOR),
        "minimax-code-cn" => ("MiniMax Code (China)", &MINIMAX_COLOR),
        "mistral" => ("Mistral", &MISTRAL_COLOR),
        "moonshot" => ("Moonshot", &MOONSHOT),
        "nanogpt" => ("NanoGPT", &NANOGPT),
        "novita" => ("Novita", &NOVITA_COLOR),
        "nvidia" => ("NVIDIA", &NVIDIA_COLOR),
        "ollama" | "ollama-cloud" => ("Ollama", &OLLAMA),
        "openai" => ("OpenAI", &OPENAI),
        "openai-codex" => ("OpenAI Codex", &OPENAI),
        "opencode-go" => ("OpenCode Go", &OPENCODE_GO),
        "opencode-zen" => ("OpenCode Zen", &OPENCODE),
        "openrouter" => ("OpenRouter", &OPENROUTER),
        "qianfan" => ("Qianfan", &BAIDUCLOUD_COLOR),
        "qwen-portal" => ("Qwen", &QWEN_COLOR),
        "sakana" => ("Sakana", &SAKANA_COLOR),
        "sambanova" => ("SambaNova", &SAMBANOVA_COLOR),
        "snowflake" => ("Snowflake", &SNOWFLAKE_COLOR),
        "stepfun" => ("StepFun", &STEPFUN_COLOR),
        "synthetic" => ("Synthetic", &SYNTHETIC),
        "together" => ("Together", &TOGETHER_COLOR),
        "venice" => ("Venice", &VENICE_COLOR),
        "vercel-ai-gateway" => ("Vercel AI Gateway", &VERCEL),
        "xai" | "xai-oauth" => ("xAI", &XAI),
        "xiaomi" => ("Xiaomi", &XIAOMI),
        "xiaomi-token-plan-ams" => ("Xiaomi (Amsterdam)", &XIAOMI),
        "xiaomi-token-plan-cn" => ("Xiaomi (China)", &XIAOMI),
        "xiaomi-token-plan-sgp" => ("Xiaomi (Singapore)", &XIAOMI),
        "zai" => ("Z.ai", &ZAI),
        "zenmux" => ("ZenMux", &ZENMUX),
        "zhipu-coding-plan" => ("Zhipu Coding Plan", &ZHIPU_COLOR),
        _ => return None,
    })
}

/// The provider's display name; unknown ids show as they are.
pub fn provider_name(provider: &str) -> &str {
    lookup(provider).map_or(provider, |(name, _)| name)
}

/// The provider's logo icon, if it has one.
pub fn provider_icon(provider: &str) -> Option<&'static Icon> {
    lookup(provider).map(|(_, logo)| logo)
}

/// The provider's logo at `size` points: monochrome marks in the text colour, coloured ones
/// in their own; a muted bot glyph for providers without a logo.
pub fn provider_logo(provider: &str, size: f32, k: &Kit) -> El {
    match provider_icon(provider) {
        Some(logo) => icon(logo, size).color(k.c.text_base),
        None => icon(&crate::icons::BOT, size).color(k.c.text_muted),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_have_names_and_logos() {
        assert_eq!(provider_name("anthropic"), "Anthropic");
        assert_eq!(provider_name("some-new-provider"), "some-new-provider");
        assert!(provider_icon("openai-codex").is_some());
        assert!(provider_icon("unknown").is_none());
    }

    #[test]
    fn every_logo_body_is_a_square_icon() {
        for (_, logo) in ["anthropic", "gitlab-duo", "nanogpt", "mistral", "openai"]
            .into_iter()
            .filter_map(lookup)
        {
            assert!(logo.view > 0.0);
            assert!(!logo.body.contains("<svg"), "{} keeps its root", logo.name);
        }
    }
}
