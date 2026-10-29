//! Steam ID не должен попадать на экран: ни в лог, ни в имена файлов облака.
//!
//! SteamID64 обычного аккаунта — 17 цифр, начинается с 7656119. В путях
//! (`CloudDir/7656119…/stats.json`) сегмент с ID убирается целиком,
//! в остальном тексте ID заменяется на `•••`.

const PREFIX: &[u8] = b"7656119";
const LEN: usize = 17;

pub fn redact(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if is_id_at(b, i) {
            let end = i + LEN;
            let seg_start = i == 0 || b[i - 1] == b'/' || b[i - 1] == b'\\';
            let seg_end = end < b.len() && (b[end] == b'/' || b[end] == b'\\');
            if seg_start && seg_end {
                i = end + 1;
            } else {
                out.push_str("•••");
                i = end;
            }
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

fn is_id_at(b: &[u8], i: usize) -> bool {
    if i + LEN > b.len() || !b[i..].starts_with(PREFIX) {
        return false;
    }
    if i > 0 && b[i - 1].is_ascii_digit() {
        return false;
    }
    if !b[i..i + LEN].iter().all(u8::is_ascii_digit) {
        return false;
    }
    b.get(i + LEN).is_none_or(|c| !c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn path_segment_is_dropped() {
        assert_eq!(redact("76561198000000001/stats.json"), "stats.json");
        assert_eq!(redact("Saves/CloudDir/76561198000000001/progression.json"), "Saves/CloudDir/progression.json");
    }

    #[test]
    fn inline_id_is_masked() {
        assert_eq!(
            redact("Caching Steam ID:  76561198000000001 [API loaded no]"),
            "Caching Steam ID:  ••• [API loaded no]"
        );
    }

    #[test]
    fn other_numbers_stay() {
        assert_eq!(redact("appid 3405340, порт 54310"), "appid 3405340, порт 54310");
        assert_eq!(redact("765611980000000012"), "765611980000000012");
        assert_eq!(redact("→ ↓ ок"), "→ ↓ ок");
    }
}
