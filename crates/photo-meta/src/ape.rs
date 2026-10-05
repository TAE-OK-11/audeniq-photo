//! APEv2 / APEv1 tags (WavPack, TTA, Monkey's Audio).

use crate::{Metadata, clean, tag_name};

/// Parse an APE tag whose footer ends at `end`. Returns the tag's total
/// size (including header) or 0 if none.
pub(crate) fn read(d: &[u8], end: usize, m: &mut Metadata) -> usize {
    if end < 32 || &d[end - 32..end - 24] != b"APETAGEX" {
        return 0;
    }
    let f = &d[end - 32..end];
    let size = u32::from_le_bytes(f[12..16].try_into().expect("4")) as usize;
    let count = u32::from_le_bytes(f[16..20].try_into().expect("4")) as usize;
    let flags = u32::from_le_bytes(f[20..24].try_into().expect("4"));
    if size < 32 || size > end {
        m.warn("invalid APE tag size");
        return 0;
    }
    let items = &d[end - size..end - 32];
    let has_header = flags & 0x8000_0000 != 0;
    let mut pos = 0;
    for _ in 0..count.min(1024) {
        if pos + 8 > items.len() {
            break;
        }
        let len = u32::from_le_bytes(items[pos..pos + 4].try_into().expect("4")) as usize;
        let iflags = u32::from_le_bytes(items[pos + 4..pos + 8].try_into().expect("4"));
        pos += 8;
        let Some(kend) = items[pos..].iter().position(|&b| b == 0) else { break };
        let key = String::from_utf8_lossy(&items[pos..pos + kend]).into_owned();
        pos += kend + 1;
        let Some(value) = items.get(pos..pos.saturating_add(len)) else {
            m.warn("truncated APE item");
            break;
        };
        pos += len;
        if (iflags >> 1) & 3 != 0 {
            continue; // binary or external locator
        }
        let name = match key.to_ascii_lowercase().as_str() {
            "tool name" => "ToolName".to_string(),
            "tool version" => "ToolVersion".to_string(),
            "encoder" => "Encoder".to_string(),
            "comment" => "Comment".to_string(),
            "description" => "Description".to_string(),
            "software" => "Software".to_string(),
            _ => tag_name(&key),
        };
        let text = clean(&String::from_utf8_lossy(value).replace('\0', "/"));
        m.text("APE", &name, text);
    }
    size + if has_header { 32 } else { 0 }
}
