//! Providers taken from their own API documentation, not yet probed from
//! here: each row is the base URL, protocol and key variable the provider's
//! docs state (read 2026-09-25; the page is named beside the row). Every
//! capability stays `Unverified` -- the docs say the route exists, nothing
//! more -- and a probe that disagrees corrects the row.
//!
//! Only fixed hosts are here. An endpoint whose URL carries the person's own
//! workspace, region, resource or account (Azure OpenAI, AWS Bedrock,
//! Alibaba Model Studio, Cloudflare Workers AI) is a custom endpoint
//! (`providers add`), because no template could spell it.

use crate::routing::wire::WireProtocol::{self, AnthropicMessages, OpenAiChat, OpenAiResponses};

use super::{Declared, Provider, unverified_support};

/// One documented provider: name, `(protocol, base URL)` pairs, the key
/// variable (empty for a local server that takes none), and its docs page.
type Row = (
    &'static str,
    &'static [(WireProtocol, &'static str)],
    &'static str,
    &'static str,
);

const ROWS: &[Row] = &[
    (
        "openai",
        &[
            (OpenAiResponses, "https://api.openai.com/v1"),
            (OpenAiChat, "https://api.openai.com/v1"),
        ],
        "OPENAI_API_KEY",
        "https://developers.openai.com/api/reference/overview",
    ),
    (
        "mistral",
        &[(OpenAiChat, "https://api.mistral.ai/v1")],
        "MISTRAL_API_KEY",
        "https://docs.mistral.ai/api/",
    ),
    (
        "deepseek",
        &[
            (OpenAiChat, "https://api.deepseek.com"),
            (AnthropicMessages, "https://api.deepseek.com/anthropic"),
        ],
        "DEEPSEEK_API_KEY",
        "https://api-docs.deepseek.com/guides/anthropic_api",
    ),
    (
        "xai",
        &[
            (OpenAiResponses, "https://api.x.ai/v1"),
            (OpenAiChat, "https://api.x.ai/v1"),
        ],
        "XAI_API_KEY",
        "https://docs.x.ai/docs/api-reference",
    ),
    (
        "moonshot",
        &[
            (OpenAiChat, "https://api.moonshot.ai/v1"),
            (OpenAiResponses, "https://api.moonshot.ai/v1"),
            (AnthropicMessages, "https://api.moonshot.ai/anthropic"),
        ],
        "MOONSHOT_API_KEY",
        "https://platform.kimi.ai/docs/guide/agent-support",
    ),
    (
        "zai-coding",
        &[
            (AnthropicMessages, "https://api.z.ai/api/anthropic"),
            (OpenAiChat, "https://api.z.ai/api/coding/paas/v4"),
        ],
        "ZAI_API_KEY",
        "https://docs.z.ai/devpack/tool/claude",
    ),
    (
        "minimax",
        &[
            (AnthropicMessages, "https://api.minimax.io/anthropic"),
            (OpenAiChat, "https://api.minimax.io/v1"),
        ],
        "MINIMAX_API_KEY",
        "https://platform.minimax.io/docs/guides/quickstart",
    ),
    (
        "together",
        &[(OpenAiChat, "https://api.together.ai/v1")],
        "TOGETHER_API_KEY",
        "https://docs.together.ai/docs/openai-api-compatibility",
    ),
    (
        "fireworks",
        &[
            (OpenAiChat, "https://api.fireworks.ai/inference/v1"),
            (AnthropicMessages, "https://api.fireworks.ai/inference"),
        ],
        "FIREWORKS_API_KEY",
        "https://docs.fireworks.ai/tools-sdks/openai-compatibility",
    ),
    (
        "cerebras",
        &[(OpenAiChat, "https://api.cerebras.ai/v1")],
        "CEREBRAS_API_KEY",
        "https://inference-docs.cerebras.ai/resources/openai",
    ),
    (
        "perplexity",
        &[(OpenAiResponses, "https://api.perplexity.ai/v1")],
        "PERPLEXITY_API_KEY",
        "https://docs.perplexity.ai/docs/agent-api/openai-compatibility",
    ),
    (
        "cohere",
        &[(OpenAiChat, "https://api.cohere.ai/compatibility/v1")],
        "COHERE_API_KEY",
        "https://docs.cohere.com/docs/compatibility-api",
    ),
    (
        "nebius",
        &[(OpenAiChat, "https://api.tokenfactory.nebius.com/v1")],
        "NEBIUS_API_KEY",
        "https://docs.tokenfactory.nebius.com",
    ),
    (
        "deepinfra",
        &[(OpenAiChat, "https://api.deepinfra.com/v1/openai")],
        "DEEPINFRA_API_KEY",
        "https://docs.deepinfra.com/chat/overview",
    ),
    (
        "sambanova",
        &[(OpenAiChat, "https://api.sambanova.ai/v1")],
        "SAMBANOVA_API_KEY",
        "https://docs.sambanova.ai/docs/en/get-started/api-keys-urls",
    ),
    (
        "novita",
        &[(OpenAiChat, "https://api.novita.ai/openai")],
        "NOVITA_API_KEY",
        "https://docs.novita.ai/guides/llm-api",
    ),
    (
        "huggingface",
        &[(OpenAiChat, "https://router.huggingface.co/v1")],
        "HF_TOKEN",
        "https://huggingface.co/docs/inference-providers/index",
    ),
    (
        "ollama-cloud",
        &[
            (OpenAiChat, "https://ollama.com/v1"),
            (OpenAiResponses, "https://ollama.com/v1"),
        ],
        "OLLAMA_API_KEY",
        "https://docs.ollama.com/api/openai-compatibility",
    ),
    (
        "vercel",
        &[
            (OpenAiChat, "https://ai-gateway.vercel.sh/v1"),
            (OpenAiResponses, "https://ai-gateway.vercel.sh/v1"),
            (AnthropicMessages, "https://ai-gateway.vercel.sh"),
        ],
        "AI_GATEWAY_API_KEY",
        "https://vercel.com/docs/ai-gateway/sdks-and-apis/openai-chat-completions",
    ),
    (
        "baseten",
        &[
            (OpenAiChat, "https://inference.baseten.co/v1"),
            (AnthropicMessages, "https://inference.baseten.co"),
        ],
        "BASETEN_API_KEY",
        "https://docs.baseten.co/development/model-apis/overview",
    ),
    (
        "mimo",
        &[
            (OpenAiChat, "https://api.xiaomimimo.com/v1"),
            (AnthropicMessages, "https://api.xiaomimimo.com/anthropic"),
        ],
        "MIMO_API_KEY",
        "https://mimo.mi.com/docs/en-US/quick-start/summary/first-api-call",
    ),
    (
        "stepfun",
        &[(OpenAiChat, "https://api.stepfun.ai/v1")],
        "STEP_API_KEY",
        "https://platform.stepfun.ai/docs/en/guides/developer/openai",
    ),
    (
        "venice",
        &[(OpenAiChat, "https://api.venice.ai/api/v1")],
        "VENICE_API_KEY",
        "https://docs.venice.ai/overview/getting-started",
    ),
    (
        "chutes",
        &[(OpenAiChat, "https://llm.chutes.ai/v1")],
        "CHUTES_API_KEY",
        "https://chutes.ai/docs",
    ),
    (
        "lm-studio",
        &[
            (OpenAiChat, "http://localhost:1234/v1"),
            (OpenAiResponses, "http://localhost:1234/v1"),
            (AnthropicMessages, "http://localhost:1234"),
        ],
        "",
        "https://lmstudio.ai/docs/developer/openai-compat",
    ),
    // Beside the native `gemini` template: this one relays OpenAI chat to
    // Google as-is, past the gateway's Gemini codec, and Sterna warns so.
    (
        "gemini-openai",
        &[(
            OpenAiChat,
            "https://generativelanguage.googleapis.com/v1beta/openai",
        )],
        "GEMINI_API_KEY",
        "https://ai.google.dev/gemini-api/docs/openai",
    ),
    (
        "vllm",
        &[
            (OpenAiChat, "http://localhost:8000/v1"),
            (OpenAiResponses, "http://localhost:8000/v1"),
            (AnthropicMessages, "http://localhost:8000"),
        ],
        "",
        "https://docs.vllm.ai/en/latest/serving/online_serving/",
    ),
];

/// The documented providers, as templates.
pub fn templates() -> Vec<Provider> {
    ROWS.iter()
        .map(|(name, protocols, key, _docs)| Provider {
            name: (*name).to_owned(),
            protocols: protocols
                .iter()
                .map(|(protocol, base)| unverified_support(*protocol, base))
                .collect(),
            model_list_endpoint: Declared::Unverified,
            usage_telemetry: Declared::Unverified,
            credential_env: if key.is_empty() {
                vec![]
            } else {
                vec![(*key).to_owned()]
            },
            headers: vec![],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_provider_has_a_unique_name_a_real_url_and_its_docs() {
        let builtin: Vec<String> = super::super::templates()
            .into_iter()
            .map(|p| p.name)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for (name, protocols, _, docs) in ROWS {
            assert!(seen.insert(*name), "{name} twice");
            assert_eq!(
                builtin.iter().filter(|n| n.as_str() == *name).count(),
                1,
                "{name} collides with a template"
            );
            assert!(docs.starts_with("https://"), "{name}");
            for (_, base) in *protocols {
                assert!(
                    base.starts_with("https://") || base.starts_with("http://localhost"),
                    "{name}: {base}"
                );
                assert!(
                    !base.ends_with('/'),
                    "{name}: a base URL carries no trailing slash"
                );
            }
        }
    }
}
