//! 协议无关的内部规范表示（Canonical Representation）

use serde::{Deserialize, Serialize};

/// 消息角色
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CanonicalRole {
    System,
    User,
    Assistant,
}

/// 内容块
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        source: CanonicalImageSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<CanonicalContentBlock>,
    },
}

/// 图片来源
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CanonicalImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
}

/// 消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: CanonicalRole,
    pub content: Vec<CanonicalContentBlock>,
}

/// 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

/// 规范请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    pub messages: Vec<CanonicalMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<Vec<CanonicalContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<CanonicalTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// 无法映射到目标协议的字段名列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmapped: Vec<String>,
}

/// 停止原因
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalStopReason {
    EndTurn,
    StopSequence,
    MaxTokens,
    ToolUse,
}

/// Token 用量
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

/// 规范响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<CanonicalContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<CanonicalStopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CanonicalUsage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_request_roundtrip() {
        let req = CanonicalRequest {
            model: "gpt-4".to_string(),
            messages: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![CanonicalContentBlock::Text {
                    text: "Hello".to_string(),
                }],
            }],
            system: Some(vec![CanonicalContentBlock::Text {
                text: "You are helpful".to_string(),
            }]),
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(100),
            stop_sequences: None,
            stream: false,
            tools: None,
            tool_choice: None,
            unmapped: vec![],
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: CanonicalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, "gpt-4");
        assert_eq!(parsed.temperature, Some(0.7));
        assert_eq!(parsed.messages.len(), 1);
        assert!(parsed.system.is_some());
    }

    #[test]
    fn test_canonical_response_roundtrip() {
        let resp = CanonicalResponse {
            id: "msg_123".to_string(),
            model: "gpt-4".to_string(),
            content: vec![CanonicalContentBlock::Text {
                text: "Hi there".to_string(),
            }],
            stop_reason: Some(CanonicalStopReason::EndTurn),
            usage: Some(CanonicalUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: Some(30),
            }),
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: CanonicalResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "msg_123");
        assert!(matches!(
            parsed.stop_reason,
            Some(CanonicalStopReason::EndTurn)
        ));
        assert!(parsed.usage.is_some());
    }

    #[test]
    fn test_canonical_role_serialization() {
        assert_eq!(
            serde_json::to_string(&CanonicalRole::User).unwrap(),
            "\"user\""
        );
        assert_eq!(
            serde_json::to_string(&CanonicalRole::Assistant).unwrap(),
            "\"assistant\""
        );
    }

    #[test]
    fn test_canonical_stop_reason_roundtrip() {
        for variant in [
            CanonicalStopReason::EndTurn,
            CanonicalStopReason::StopSequence,
            CanonicalStopReason::MaxTokens,
            CanonicalStopReason::ToolUse,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let parsed: CanonicalStopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, parsed);
        }
    }

    #[test]
    fn test_image_source_base64() {
        let img = CanonicalContentBlock::Image {
            source: CanonicalImageSource::Base64 {
                media_type: "image/png".to_string(),
                data: "iVBORw0KGgo=".to_string(),
            },
        };
        let json = serde_json::to_string(&img).unwrap();
        assert!(json.contains("image/png"));
        assert!(json.contains("iVBORw0KGgo="));
    }

    #[test]
    fn test_image_source_url() {
        let img = CanonicalContentBlock::Image {
            source: CanonicalImageSource::Url {
                url: "https://example.com/image.png".to_string(),
            },
        };
        let json = serde_json::to_string(&img).unwrap();
        assert!(json.contains("https://example.com/image.png"));
    }

    #[test]
    fn test_tool_use_block() {
        let block = CanonicalContentBlock::ToolUse {
            id: "call_123".to_string(),
            name: "search".to_string(),
            input: serde_json::json!({"query": "test"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("call_123"));
        assert!(json.contains("search"));
    }
}
