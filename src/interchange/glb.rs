//! The binary glTF container: a JSON chunk and a binary chunk.

use thiserror::Error;

const MAGIC: u32 = 0x4654_6C67; // "glTF"
const VERSION: u32 = 2;
const JSON: u32 = 0x4E4F_534A; // "JSON"
const BIN: u32 = 0x004E_4942; // "BIN\0"

#[derive(Debug, Error)]
pub enum GlbError {
    #[error("not a binary glTF")]
    NotGlb,
    #[error("truncated binary glTF")]
    Truncated,
    #[error("glTF JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Writes a `.glb`: `json` (the glTF document) and `bin` (its one buffer, without a `uri`).
///
/// # Errors
///
/// When the document doesn't serialize.
pub fn write_glb(json: &serde_json::Value, bin: &[u8]) -> Result<Vec<u8>, GlbError> {
    let mut text = serde_json::to_vec(json)?;
    // Chunks are 4-byte aligned: JSON pads with spaces, the binary chunk with zeros.
    text.resize(text.len().next_multiple_of(4), b' ');
    let mut bin = bin.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let chunk = |len: usize| u32::try_from(len).unwrap_or(u32::MAX);
    let total = 12 + 8 + text.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    for word in [MAGIC, VERSION, chunk(total), chunk(text.len()), JSON] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.extend_from_slice(&text);
    if !bin.is_empty() {
        out.extend_from_slice(&chunk(bin.len()).to_le_bytes());
        out.extend_from_slice(&BIN.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    Ok(out)
}

/// Splits a `.glb` into its JSON document and its binary chunk (empty when it has none).
///
/// # Errors
///
/// When `bytes` isn't a binary glTF or its JSON doesn't parse.
pub fn read_glb(bytes: &[u8]) -> Result<(serde_json::Value, &[u8]), GlbError> {
    let word = |at: usize| -> Result<u32, GlbError> {
        let b = bytes.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).ok_or(GlbError::Truncated)?;
        Ok(u32::from_le_bytes(b))
    };
    if word(0)? != MAGIC {
        return Err(GlbError::NotGlb);
    }
    let len = |w: u32| usize::try_from(w).map_err(|_| GlbError::Truncated);
    let json_len = len(word(12)?)?;
    if word(16)? != JSON {
        return Err(GlbError::NotGlb);
    }
    let json = bytes.get(20..20 + json_len).ok_or(GlbError::Truncated)?;
    let doc = serde_json::from_slice(json)?;
    let next = 20 + json_len;
    let bin = if bytes.len() >= next + 8 && word(next + 4)? == BIN {
        let bin_len = len(word(next)?)?;
        bytes.get(next + 8..next + 8 + bin_len).ok_or(GlbError::Truncated)?
    } else {
        &[]
    };
    Ok((doc, bin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let doc = serde_json::json!({ "asset": { "version": "2.0" } });
        let bytes = write_glb(&doc, &[1, 2, 3]).unwrap();
        assert_eq!(bytes.len() % 4, 0);
        let (back, bin) = read_glb(&bytes).unwrap();
        assert_eq!(back, doc);
        assert_eq!(&bin[..3], &[1, 2, 3]);
    }
}
