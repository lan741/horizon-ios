//! Blockstate compilation: variants and multipart → one binary table.
//!
//! Minecraft resolves a block's visual state at runtime by matching property
//! strings. We do that once, on import, and emit:
//! ```text
//! "BS1" | u32 state_count
//! per state: u8 rot_x/90 | u8 rot_y/90 | u8 flags | u8 weight
//!            u16 prop_count | (str16 name, str16 value)*
//!            str16 model_id
//! ```
//! `flags` bit0 = uvlock, bit1 = multipart entry (properties then carry a
//! single `$when` pair holding the condition JSON).

use hz_core::{Buf, Json};

use crate::parse_props;

fn rot(v: Option<&Json>) -> u8 {
    let d = v.and_then(|x| x.as_f64()).unwrap_or(0.0);
    ((d / 90.0).round() as i64).rem_euclid(4) as u8
}

pub fn compile(j: &Json) -> Result<Vec<u8>, String> {
    let mut buf = Buf::new();
    let mut states: Vec<(u8, u8, u8, u8, Vec<(String, String)>, String)> = Vec::new();

    if let Some(variants) = j.get("variants").and_then(|v| v.as_obj()) {
        for (key, val) in variants {
            let props = parse_props(key);
            let items: Vec<&Json> = match val {
                Json::Arr(a) => a.iter().collect(),
                other => vec![other],
            };
            for item in items {
                let model = item
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("minecraft:block/air")
                    .to_string();
                let uvlock = item
                    .get("uvlock")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                let weight = item
                    .get("weight")
                    .and_then(|w| w.as_f64())
                    .unwrap_or(1.0)
                    .clamp(0.0, 255.0) as u8;
                states.push((
                    rot(item.get("x")),
                    rot(item.get("y")),
                    uvlock as u8,
                    weight,
                    props.clone(),
                    model,
                ));
            }
        }
    } else if let Some(parts) = j.get("multipart").and_then(|v| v.as_arr()) {
        for part in parts {
            let when = part.get("when").cloned().unwrap_or(Json::Null);
            let applies: Vec<&Json> = match part.get("apply") {
                Some(Json::Arr(a)) => a.iter().collect(),
                Some(other) => vec![other],
                None => continue,
            };
            for item in applies {
                let model = item
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or("minecraft:block/air")
                    .to_string();
                let uvlock = item
                    .get("uvlock")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false);
                states.push((
                    rot(item.get("x")),
                    rot(item.get("y")),
                    (uvlock as u8) | 0b10,
                    1,
                    vec![("$when".to_string(), when.to_json_string())],
                    model,
                ));
            }
        }
    } else {
        return Err("neither 'variants' nor 'multipart'".to_string());
    }

    buf.bytes(b"BS1");
    buf.u32(states.len() as u32);
    for (rx, ry, flags, weight, props, model) in states {
        buf.u8(rx);
        buf.u8(ry);
        buf.u8(flags);
        buf.u8(weight);
        buf.u16(props.len() as u16);
        for (k, v) in &props {
            buf.str16(k);
            buf.str16(v);
        }
        buf.str16(&model);
    }
    Ok(buf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hz_core::util::Reader;

    #[test]
    fn variants_and_multipart() {
        let j = Json::parse(
            r#"{"variants":{"facing=north,half=top":{"model":"minecraft:block/cube","x":90},
                            "facing=south":{"model":"minecraft:block/cube","y":180,"uvlock":true}}}"#,
        )
        .unwrap();
        let bin = compile(&j).unwrap();
        let mut r = Reader::new(&bin);
        assert_eq!(r.take(3).unwrap(), b"BS1");
        assert_eq!(r.u32().unwrap(), 2);
        assert_eq!(r.u8().unwrap(), 1); // 90/90
        assert_eq!(r.u8().unwrap(), 0);
        let flags_second_pos = 4 + 4 + 1 + 1;
        assert_eq!(bin[flags_second_pos], 1); // uvlock
    }

    #[test]
    fn missing_both_is_error() {
        let j = Json::parse("{}").unwrap();
        assert!(compile(&j).is_err());
    }
}
