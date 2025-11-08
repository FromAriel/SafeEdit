use std::fmt;

use anyhow::{Result, anyhow};
use chardetng::EncodingDetector;
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingSource {
    Override,
    Bom,
    Detector,
    AssumedUtf8,
}

impl fmt::Display for EncodingSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            EncodingSource::Override => "override",
            EncodingSource::Bom => "bom",
            EncodingSource::Detector => "detector",
            EncodingSource::AssumedUtf8 => "assumed-utf8",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone)]
pub struct EncodingDecision {
    pub encoding: &'static Encoding,
    pub source: EncodingSource,
}

#[derive(Debug, Clone)]
pub struct DecodedText {
    pub text: String,
    pub had_errors: bool,
    pub decision: EncodingDecision,
}

#[derive(Debug, Clone)]
pub struct EncodingStrategy {
    override_encoding: Option<&'static Encoding>,
    override_label: Option<String>,
}

impl EncodingStrategy {
    pub fn new(override_label: Option<&str>) -> Result<Self> {
        if let Some(label) = override_label {
            let trimmed = label.trim();
            let encoding = Encoding::for_label(trimmed.as_bytes())
                .ok_or_else(|| anyhow!("unknown encoding override '{trimmed}'"))?;
            Ok(Self {
                override_encoding: Some(encoding),
                override_label: Some(trimmed.to_string()),
            })
        } else {
            Ok(Self {
                override_encoding: None,
                override_label: None,
            })
        }
    }

    pub fn describe(&self) -> String {
        if let (Some(label), Some(enc)) = (&self.override_label, self.override_encoding) {
            format!(
                "override '{}' ({}), auto-detect disabled",
                label,
                enc.name()
            )
        } else {
            "auto-detect (BOM → detector → UTF-8)".to_string()
        }
    }

    pub fn decide(&self, bytes: &[u8]) -> EncodingDecision {
        if let Some(encoding) = self.override_encoding {
            return EncodingDecision {
                encoding,
                source: EncodingSource::Override,
            };
        }

        detect_auto(bytes)
    }

    pub fn decode(&self, bytes: &[u8]) -> DecodedText {
        let decision = self.decide(bytes);
        let (cow, _encoding_used, had_errors) = decision.encoding.decode(bytes);
        DecodedText {
            text: cow.into_owned(),
            had_errors,
            decision,
        }
    }
}

fn detect_auto(bytes: &[u8]) -> EncodingDecision {
    if let Some(encoding) = detect_bom(bytes) {
        return EncodingDecision {
            encoding,
            source: EncodingSource::Bom,
        };
    }

    if std::str::from_utf8(bytes).is_ok() {
        return EncodingDecision {
            encoding: UTF_8,
            source: EncodingSource::AssumedUtf8,
        };
    }

    let mut detector = EncodingDetector::new();
    detector.feed(bytes, true);
    let encoding = detector.guess(None, true);

    EncodingDecision {
        encoding,
        source: EncodingSource::Detector,
    }
}

fn detect_bom(bytes: &[u8]) -> Option<&'static Encoding> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some(UTF_8);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return Some(UTF_16LE);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return Some(UTF_16BE);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_validation() {
        let strategy = EncodingStrategy::new(Some("utf-16le")).expect("valid encoding");
        assert_eq!(strategy.override_label.as_deref(), Some("utf-16le"));
    }

    #[test]
    fn utf8_detection_without_bom() {
        let data = b"hello world";
        let decision = detect_auto(data);
        assert_eq!(decision.source, EncodingSource::AssumedUtf8);
        assert_eq!(decision.encoding.name(), "UTF-8");
    }

    #[test]
    fn bom_detection_takes_precedence() {
        let data = [0xFF, 0xFE, 0x61, 0x00];
        let decision = detect_auto(&data);
        assert_eq!(decision.source, EncodingSource::Bom);
        assert_eq!(decision.encoding.name(), "UTF-16LE");
    }
}
