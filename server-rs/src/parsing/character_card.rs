// 角色卡 V2 解析:PNG tEXt/iTXt 内嵌 或 独立 JSON,未知字段无损保留
// 与 Node 版(png-chunks-extract + latin1)行为对齐
use base64::Engine;
use serde_json::Value;
use std::io::Read;

pub struct ParsedCard {
    /// 完整 V2 JSON 对象(含未知字段)
    pub data: Value,
    /// 兜底名称:data.name 缺失/非字符串时用文件名去扩展名
    pub name: String,
    /// PNG 卡 → 原图字节;JSON 卡 → None
    pub avatar: Option<Vec<u8>>,
}

impl std::fmt::Debug for ParsedCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedCard")
            .field("name", &self.name)
            .field("has_data", &self.data.is_object())
            .field("avatar_bytes", &self.avatar.as_ref().map(|v| v.len()))
            .finish()
    }
}

const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
/// JPEG:FF D8 FF
const JPEG_SIG: [u8; 3] = [0xFF, 0xD8, 0xFF];
/// WebP:RIFF .... WEBP
const RIFF_SIG: [u8; 4] = *b"RIFF";
const WEBP_SIG: [u8; 4] = *b"WEBP";

pub fn parse_character_card(buffer: &[u8], original_name: &str) -> Result<ParsedCard, String> {
    if buffer.len() >= 8 && buffer[..8] == PNG_SIG {
        parse_png_card(buffer, original_name)
    } else if buffer.len() >= 3 && buffer[..3] == JPEG_SIG {
        parse_jpeg_card(buffer, original_name)
    } else if buffer.len() >= 12 && buffer[..4] == RIFF_SIG && buffer[8..12] == WEBP_SIG {
        parse_webp_card(buffer, original_name)
    } else {
        // 非图像:按 UTF-8 字符串直接解析(忽略扩展名,更宽容)
        let text = String::from_utf8_lossy(buffer);
        let data = parse_chara_payload(&text)
            .ok_or_else(|| format!("无法解析角色卡 \"{original_name}\":不是有效的角色卡图片(PNG/JPEG/WebP)或 V2 规范 JSON,且图片中未检测到内嵌角色数据"))?;
        let name = fallback_name(&data, original_name);
        Ok(ParsedCard {
            data,
            name,
            avatar: None,
        })
    }
}

/// JPEG:遍历段,在 APP1/Exif/COM 等段中查找含 "chara" 的负载并解析
fn parse_jpeg_card(buffer: &[u8], original_name: &str) -> Result<ParsedCard, String> {
    let mut pos = 2usize;
    let mut found: Option<Vec<u8>> = None;
    while pos + 4 <= buffer.len() {
        if buffer[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = buffer[pos + 1];
        if marker == 0xFF {
            pos += 2;
            continue;
        }
        // SOS(0xDA)之后为压缩数据,停止;EOI(0xD9)结束
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            // 无长度段(单字节 marker)
            pos += 2;
            continue;
        }
        if pos + 4 > buffer.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([buffer[pos + 2], buffer[pos + 3]]) as usize;
        if seg_len < 2 {
            break;
        }
        let payload_start = pos + 4;
        let payload_end = (payload_start + seg_len - 2).min(buffer.len());
        if payload_start <= payload_end {
            let payload = &buffer[payload_start..payload_end];
            if let Some(raw) = find_chara_in_bytes(payload) {
                found = Some(raw.to_vec());
                break;
            }
        }
        pos = pos + 2 + seg_len;
    }
    match found {
        Some(raw) => {
            let value_text = String::from_utf8_lossy(&raw);
            let data = parse_chara_payload(&value_text)
                .ok_or_else(|| format!("无法解析角色卡 \"{original_name}\":JPEG 内嵌角色数据不是有效的 V2 规范 JSON"))?;
            let name = fallback_name(&data, original_name);
            Ok(ParsedCard { data, name, avatar: Some(buffer.to_vec()) })
        }
        None => Err(format!("无法解析角色卡 \"{original_name}\":JPEG 图片中未检测到内嵌角色数据(chara),请确认这是 SillyTavern 导出的角色卡图片")),
    }
}

/// WebP:遍历 chunk(4 字节 FourCC + 4 字节小端长度),查找角色数据 chunk。
/// 生态内该 chunk 名写作 "chara"(5 字符),其前 4 字节为 "char";规范 FourCC 为 4 字节,
/// 故统一按 4 字节读取并匹配 "char" 前缀。
fn parse_webp_card(buffer: &[u8], original_name: &str) -> Result<ParsedCard, String> {
    let mut pos = 12usize; // 跳过 RIFF 头
    let mut found: Option<Vec<u8>> = None;
    while pos + 8 <= buffer.len() {
        let fourcc = &buffer[pos..pos + 4];
        let size = u32::from_le_bytes([
            buffer[pos + 4],
            buffer[pos + 5],
            buffer[pos + 6],
            buffer[pos + 7],
        ]) as usize;
        let data_start = pos + 8;
        let data_end = (data_start + size).min(buffer.len());
        if fourcc == b"char" {
            found = Some(buffer[data_start..data_end].to_vec());
            break;
        }
        let next = data_end + (size % 2); // chunk 按 2 字节对齐
        if next <= pos {
            break; // 无法前进(畸形文件),终止避免死循环
        }
        pos = next;
    }
    match found {
        Some(raw) => {
            let value_text = String::from_utf8_lossy(&raw);
            let data = parse_chara_payload(&value_text)
                .ok_or_else(|| format!("无法解析角色卡 \"{original_name}\":WebP 内嵌角色数据不是有效的 V2 规范 JSON"))?;
            let name = fallback_name(&data, original_name);
            Ok(ParsedCard { data, name, avatar: Some(buffer.to_vec()) })
        }
        None => Err(format!("无法解析角色卡 \"{original_name}\":WebP 图片中未检测到内嵌角色数据(chara),请确认这是 SillyTavern 导出的角色卡图片")),
    }
}

/// 在一段字节负载中查找 "chara" 关键字的负载
/// SillyTavern PNG 卡是 keyword \0 value;JPEG 生态常把 chara 写入 APP1/COM,
/// 负载内会出现 "\x00chara\x00" 或 "chara" 后跟 JSON/Base64。此处优先
/// 按 PNG 同款 "chara\0" 分割;若无 \0 分隔,则取段内第一个 JSON 对象。
fn find_chara_in_bytes(payload: &[u8]) -> Option<&[u8]> {
    // 方式 1:寻找 "\0chara\0" 或 "chara\0" 分隔
    for pat in [b"\x00chara\x00".as_slice(), b"chara\x00".as_slice()] {
        if let Some(idx) = find_subslice(payload, pat) {
            let after = &payload[idx + pat.len()..];
            return Some(after);
        }
    }
    // 方式 2:直接找 "{...}" 形式 JSON(以 { 开头的最外层 JSON)
    if let Some(start) = payload.iter().position(|&b| b == b'{') {
        let candidate = &payload[start..];
        if serde_json::from_str::<Value>(&String::from_utf8_lossy(candidate)).is_ok() {
            return Some(candidate);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_png_card(buffer: &[u8], original_name: &str) -> Result<ParsedCard, String> {
    let mut pos = 8usize;
    let mut chara_value: Option<Vec<u8>> = None;
    while pos + 8 <= buffer.len() {
        let len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        let ctype = &buffer[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start + len;
        if data_end + 4 > buffer.len() {
            break; // 截断的 PNG,忽略剩余
        }
        if ctype == b"tEXt" || ctype == b"iTXt" {
            if let Some((keyword, value)) = split_keyword_value(&buffer[data_start..data_end]) {
                if keyword == "chara" {
                    chara_value = Some(value.to_vec());
                }
            }
        }
        pos = data_end + 4; // 跳过 CRC
    }
    let Some(raw) = chara_value else {
        return Err(format!("无法解析角色卡 \"{original_name}\":PNG 图片中未检测到内嵌角色数据(chara),请确认这是 SillyTavern 导出的角色卡图片"));
    };
    let value_text = String::from_utf8_lossy(&raw);
    let data = parse_chara_payload(&value_text).ok_or_else(|| {
        format!("无法解析角色卡 \"{original_name}\":不是有效的 PNG 或 V2 规范 JSON")
    })?;
    let name = fallback_name(&data, original_name);
    Ok(ParsedCard {
        data,
        name,
        avatar: Some(buffer.to_vec()),
    })
}

/// 按第一个 \0 分割 keyword 与 value(与 Node 版 latin1 分割逻辑一致)
fn split_keyword_value(data: &[u8]) -> Option<(String, Vec<u8>)> {
    let nul = data.iter().position(|&b| b == 0)?;
    let keyword = String::from_utf8_lossy(&data[..nul]).to_string();
    let value = data[nul + 1..].to_vec();
    Some((keyword, value))
}

/// 先直接 JSON 解析,失败后尝试 base64 解码再解析;V1 兼容补 spec
fn parse_chara_payload(trimmed: &str) -> Option<Value> {
    let t = trimmed.trim();
    // 尝试 1:直接 JSON
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if v.is_object() {
            return Some(finalize_card(v));
        }
        return None;
    }
    // 尝试 2:base64 解码后 JSON
    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(t.trim()) {
        if let Ok(text) = String::from_utf8(decoded) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if v.is_object() {
                    return Some(finalize_card(v));
                }
            }
        }
    }
    None
}

/// V1 兼容:spec 缺失时补 'chara_card_v1'(无损补全)
fn ensure_spec(mut v: Value) -> Value {
    if v.get("spec").is_none() {
        if let Some(obj) = v.as_object_mut() {
            obj.insert("spec".to_string(), Value::String("chara_card_v1".into()));
        }
    }
    v
}

/// 角色卡解析收尾:补 spec + V3 data 归一化(幂等)。
fn finalize_card(v: Value) -> Value {
    let v = ensure_spec(v);
    flatten_v3_data(v)
}

/// V3 规范兼容:chara_card_v3 卡的正文存放在 data 子对象内(description/personality/scenario/
/// alternate_greetings/extensions/character_book 等),顶层 V2 字段往往为空。
/// 为了让现有消费端(引擎人设注入、侧边栏/前端展示、世界书/正则脚本/插件检测)按 V2 平铺
/// 结构读取,将 data 子对象中「顶层缺失或为空」的字段提升到顶层;顶层已有非空值的不覆盖。
/// data 子对象本身保留(未知字段无损),spec 保持 v3。幂等:二次调用时顶层已补全,不再变更。
pub fn flatten_v3_data(mut v: Value) -> Value {
    let is_v3 = matches!(
        v.get("spec").and_then(|s| s.as_str()),
        Some("chara_card_v3")
    );
    if !is_v3 {
        return v;
    }
    let Some(data) = v.get("data").cloned() else {
        return v;
    };
    let Some(data_obj) = data.as_object() else {
        return v;
    };
    let Some(root) = v.as_object_mut() else {
        return v;
    };
    for (key, val) in data_obj {
        let needs_fill = match root.get(key) {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            Some(Value::Object(o)) => o.is_empty(),
            Some(_) => false,
        };
        if needs_fill {
            root.insert(key.clone(), val.clone());
        }
    }
    v
}

/// data.name 缺失或非字符串时:文件名去扩展名 || "未命名角色"
fn fallback_name(data: &Value, original_name: &str) -> String {
    match data.get("name") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            let exact = strip_image_ext(original_name);
            if exact.is_empty() {
                "未命名角色".to_string()
            } else {
                exact
            }
        }
    }
}

fn strip_image_ext(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.ends_with(".png") {
        name[..name.len() - 4].to_string()
    } else if lower.ends_with(".json") {
        name[..name.len() - 5].to_string()
    } else {
        name.to_string()
    }
}

/// 清洗文件名:非 [\w.\u4e00-\u9fff-] 字符替换为 _
pub fn safe_file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || c == '_'
                || c == '.'
                || c == '-'
                || ('\u{4e00}'..='\u{9fff}').contains(&c)
            {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 从任意 Reader 读取完整字节(用于上传)
pub fn read_all<R: Read>(mut r: R) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造最小 JPEG:SOI + APP1 段(含 chara 负载) + EOI
    fn build_jpeg_with_chara(chara_value: &str) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF]; // SOI + 部分标记
        b.push(0xE1); // APP1
        let payload = format!("Exif\0\0{}", chara_value);
        let seg_len = (payload.len() + 2) as u16;
        b.extend_from_slice(&seg_len.to_be_bytes());
        b.extend_from_slice(payload.as_bytes());
        b.extend_from_slice(&[0xFF, 0xD9]); // EOI
        b
    }

    /// 从 JPEG 提取 chara 并解析
    #[test]
    fn parses_jpeg_card_with_chara() {
        let card = json!({
            "spec": "chara_card_v2", "spec_version": "1.0",
            "name": "JPEG角色", "description": "来自 JPEG", "first_mes": "你好"
        });
        let buffer = build_jpeg_with_chara(&card.to_string());
        let parsed = parse_character_card(&buffer, "卡.png").unwrap(); // 文件名即使 .png 也按魔数走 JPEG
        assert_eq!(parsed.name, "JPEG角色");
        assert_eq!(parsed.data["description"], json!("来自 JPEG"));
        assert!(parsed.avatar.is_some());
    }

    /// 纯 JPEG(无 chara 段)报明确错误
    #[test]
    fn rejects_jpeg_without_chara() {
        let buffer = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46, 0x49, 0x46, 0xFF, 0xD9,
        ];
        let err = parse_character_card(&buffer, "卡.png").unwrap_err();
        assert!(err.contains("chara"), "报错应提示缺少 chara 数据: {err}");
    }

    /// 构造最小 WebP:RIFF 头 + chara chunk(4 字节 FourCC "char",见 parse_webp_card 说明)
    fn build_webp_with_chara(chara_value: &str) -> Vec<u8> {
        let chunk_body = chara_value.as_bytes();
        // 文件体:4(WEBP) + 8(VP8 chunk 头+0 长度占位) + 8+body(chara chunk)
        let file_len = 4 + 8 + 8 + chunk_body.len();
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(file_len as u32).to_le_bytes());
        b.extend_from_slice(b"WEBP");
        b.extend_from_slice(b"VP8 "); // 占位图片 chunk
        b.extend_from_slice(&(0u32).to_le_bytes());
        b.extend_from_slice(b"char");
        b.extend_from_slice(&(chunk_body.len() as u32).to_le_bytes());
        b.extend_from_slice(chunk_body);
        b
    }

    /// 从 WebP 提取 chara 并解析
    #[test]
    fn parses_webp_card_with_chara() {
        let card = json!({ "spec": "chara_card_v2", "name": "WebP角色", "description": "webp" });
        let buffer = build_webp_with_chara(&card.to_string());
        let parsed = parse_character_card(&buffer, "卡.webp").unwrap();
        assert_eq!(parsed.name, "WebP角色");
    }

    /// 纯 WebP(无 chara)报明确错误
    #[test]
    fn rejects_webp_without_chara() {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(20u32).to_le_bytes());
        b.extend_from_slice(b"WEBP");
        b.extend_from_slice(b"VP8 ");
        b.extend_from_slice(&(0u32).to_le_bytes());
        let err = parse_character_card(&b, "卡.webp").unwrap_err();
        assert!(err.contains("chara"), "报错应提示缺少 chara 数据: {err}");
    }

    /// 纯 PNG 无 chara 也报明确错误
    #[test]
    fn rejects_png_without_chara() {
        // 最小 PNG 签名 + 一个 tEXt 非 chara + IEND
        let mut b = Vec::new();
        b.extend_from_slice(&PNG_SIG);
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x09]); // len
        b.extend_from_slice(b"tEXt");
        b.extend_from_slice(b"Author\0me");
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // crc 占位
        b.extend_from_slice(b"IEND");
        let err = parse_character_card(&b, "卡.png").unwrap_err();
        assert!(err.contains("chara"), "报错应提示缺少 chara 数据: {err}");
    }

    /// 构造最小 PNG:tEXt chunk(keyword\0value)+ IEND
    fn build_png_with_text(keyword: &str, value: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&PNG_SIG);
        let payload = format!("{keyword}\0{value}");
        b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        b.extend_from_slice(b"tEXt");
        b.extend_from_slice(payload.as_bytes());
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // crc 占位
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // IEND len
        b.extend_from_slice(b"IEND");
        b
    }

    /// V3 卡:顶层 V2 字段为空,正文在 data 子对象内。
    /// 解析后应把 data 的 description/personality/scenario/备用开场白/扩展/世界书补到顶层,
    /// 让现有消费端(引擎/侧边栏/前端)按 V2 平铺结构读取;spec 保持 v3,data 子对象保留(无损)。
    #[test]
    fn parses_v3_card_with_data_flattened() {
        let card = json!({
            "spec": "chara_card_v3", "spec_version": "3.0",
            "name": "V3测试", "description": "",
            "personality": "", "scenario": "",
            "first_mes": "你好",
            "data": {
                "name": "V3测试",
                "description": "背景设定在data子对象里",
                "personality": "性格在data里",
                "scenario": "情景在data里",
                "alternate_greetings": ["开场B", "开场C"],
                "extensions": { "tavern_helper": { "enabled": true } },
                "character_book": { "name": "book", "entries": [ { "id": 0, "content": "x", "constant": true } ] }
            }
        });
        let parsed = parse_character_card(card.to_string().as_bytes(), "卡.json").unwrap();
        let d = &parsed.data;
        assert_eq!(d["spec"], "chara_card_v3", "spec 应保持 v3");
        assert_eq!(d["description"], "背景设定在data子对象里", "V3 data.description 应补到顶层");
        assert_eq!(d["personality"], "性格在data里");
        assert_eq!(d["scenario"], "情景在data里");
        assert_eq!(d["alternate_greetings"].as_array().unwrap().len(), 2, "备用开场白应补到顶层");
        assert_eq!(d["extensions"]["tavern_helper"]["enabled"], json!(true), "扩展应补到顶层");
        assert_eq!(d["character_book"]["entries"].as_array().unwrap().len(), 1, "世界书应补到顶层");
        // data 子对象保留(无损)
        assert_eq!(d["data"]["description"], "背景设定在data子对象里");
        // 顶层非空字段不被覆盖
        assert_eq!(d["first_mes"], "你好");
    }

    /// V3 卡经 base64 内嵌在 PNG tEXt(char)chunk:解析后同样补全(data 与明文路径一致)
    #[test]
    fn parses_v3_card_base64_in_png_text() {
        use base64::Engine;
        let card = json!({
            "spec": "chara_card_v3", "spec_version": "3.0",
            "name": "B64V3", "description": "",
            "data": {
                "name": "B64V3",
                "description": "base64内嵌的V3卡正文",
                "character_book": { "name": "b", "entries": [] }
            }
        });
        let b64 = base64::engine::general_purpose::STANDARD.encode(card.to_string());
        let png = build_png_with_text("chara", &b64);
        let parsed = parse_character_card(&png, "卡.png").unwrap();
        assert_eq!(parsed.data["description"], "base64内嵌的V3卡正文");
        assert_eq!(parsed.data["spec"], "chara_card_v3");
    }

    /// V2 卡无 data 子对象:原样保留,不引入任何字段
    #[test]
    fn v2_card_untouched() {
        let card = json!({
            "spec": "chara_card_v2", "spec_version": "1.0",
            "name": "V2角色", "description": "顶层描述", "personality": "顶层个性"
        });
        let parsed = parse_character_card(card.to_string().as_bytes(), "卡.json").unwrap();
        assert_eq!(parsed.data["description"], "顶层描述");
        assert_eq!(parsed.data["personality"], "顶层个性");
        assert!(parsed.data.get("alternate_greetings").is_none());
        assert!(parsed.data.get("data").is_none());
    }

    /// 顶层已有非空值(如 V2/V3 混合卡冗余副本):保持顶层,不被 data 覆盖
    #[test]
    fn v3_flatten_does_not_override_nonempty_top() {
        let card = json!({
            "spec": "chara_card_v3", "spec_version": "3.0",
            "name": "混合", "description": "顶层完整描述",
            "data": { "name": "混合", "description": "data里另一份" }
        });
        let parsed = parse_character_card(card.to_string().as_bytes(), "卡.json").unwrap();
        assert_eq!(parsed.data["description"], "顶层完整描述");
    }
}
