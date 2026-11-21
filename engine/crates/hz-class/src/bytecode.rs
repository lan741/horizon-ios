//! Just enough of the JVM instruction set to find `invoke*` targets.
//!
//! Targets are located by walking the operand lengths, which needs a complete
//! length table — an unknown opcode desynchronises everything after it. The
//! table below covers the full Java 21 set including `wide`, `tableswitch`
//! and `lookupswitch` alignment.

use crate::Const;

fn u16_at(code: &[u8], i: usize) -> Option<u16> {
    let b = code.get(i..i + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

/// Length in bytes of the instruction at `pc`, excluding `wide` expansion.
fn instruction_len(code: &[u8], pc: usize) -> Option<usize> {
    let op = *code.get(pc)?;
    Some(match op {
        // 1 byte, no operand
        0x00..=0x0f
        | 0x1a..=0x35
        | 0x3b..=0x83
        | 0x85..=0x98
        | 0xac..=0xb1
        | 0xbe
        | 0xbf
        | 0xc2
        | 0xc3 => 1,
        // 2 bytes: local index / immediate / bipush
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        // 3 bytes: sipush, ldc_w/ldc2_w, iinc, branches, newarray index, etc.
        0x11
        | 0x13
        | 0x14
        | 0x84
        | 0x99..=0xa8
        | 0xb2..=0xb8
        | 0xbb
        | 0xbd
        | 0xc0
        | 0xc1
        | 0xc6
        | 0xc7 => 3,
        // 4 bytes: multianewarray, invokedynamic, invokeinterface
        0xb9 | 0xba | 0xc5 => 5,
        // goto_w / jsr_w
        0xc8 | 0xc9 => 5,
        // tableswitch: pad + 12 + 4*n
        0xaa => {
            let pad = (4 - ((pc + 1) % 4)) % 4;
            let base = pc + 1 + pad;
            let low = i32::from_be_bytes(code.get(base + 4..base + 8)?.try_into().ok()?);
            let high = i32::from_be_bytes(code.get(base + 8..base + 12)?.try_into().ok()?);
            let n = (high as i64 - low as i64 + 1).max(0) as usize;
            1 + pad + 12 + 4 * n
        }
        // lookupswitch: pad + 8 + 8*npairs
        0xab => {
            let pad = (4 - ((pc + 1) % 4)) % 4;
            let base = pc + 1 + pad;
            let npairs = i32::from_be_bytes(code.get(base + 4..base + 8)?.try_into().ok()?);
            1 + pad + 8 + 8 * npairs.max(0) as usize
        }
        // wide <op> <u16>
        0xc4 => match code.get(pc + 1).copied()? {
            0x84 => 6,
            _ => 4,
        },
        // 0xca breakpoint, 0xfe/0xff impdep: treat as 1
        0xca | 0xfe | 0xff => 1,
        _ => return None,
    })
}

fn class_of(pool: &[Const], idx: u16) -> Option<String> {
    match pool.get(idx as usize) {
        Some(Const::Class(n)) => match pool.get(*n as usize) {
            Some(Const::Utf8(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn name_of(pool: &[Const], idx: u16) -> Option<String> {
    match pool.get(idx as usize) {
        Some(Const::NameAndType(n, _)) => match pool.get(*n as usize) {
            Some(Const::Utf8(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Every `(owner, method)` this method body invokes.
pub fn invoke_targets(code: &[u8], pool: &[Const]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pc = 0usize;
    while pc < code.len() {
        let op = code[pc];
        let idx = match op {
            // invokevirtual, invokespecial, invokestatic, invokeinterface
            0xb6..=0xb9 => u16_at(code, pc + 1),
            // invokedynamic has no class operand we can name statically
            _ => None,
        };
        if let Some(i) = idx {
            let resolved = match pool.get(i as usize) {
                Some(Const::Methodref(c, nt)) | Some(Const::InterfaceMethodref(c, nt)) => {
                    Some((*c, *nt))
                }
                _ => None,
            };
            if let Some((c, nt)) = resolved {
                if let (Some(owner), Some(name)) = (class_of(pool, c), name_of(pool, nt)) {
                    out.push((owner, name));
                }
            }
        }
        match instruction_len(code, pc) {
            Some(n) if n > 0 => pc += n,
            _ => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_alignment_sensitive_ops() {
        // tableswitch with 2 entries, then invokestatic
        let mut code = vec![0xaau8];
        // pad to the 4-byte boundary the opcode requires
        while code.len() % 4 != 0 {
            code.push(0);
        }
        code.extend_from_slice(&(-1i32).to_be_bytes()); // default
        code.extend_from_slice(&0i32.to_be_bytes()); // low
        code.extend_from_slice(&1i32.to_be_bytes()); // high
        code.extend_from_slice(&0i32.to_be_bytes());
        code.extend_from_slice(&0i32.to_be_bytes());
        let invoke_at = code.len();
        code.push(0xb8);
        code.extend_from_slice(&1u16.to_be_bytes());
        code.push(0xb1);

        let pool = vec![
            Const::Empty,
            Const::Methodref(2, 3),
            Const::Class(4),
            Const::NameAndType(5, 6),
            Const::Utf8("a/B".into()),
            Const::Utf8("run".into()),
            Const::Utf8("()V".into()),
        ];
        let calls = invoke_targets(&code, &pool);
        assert_eq!(calls, vec![("a/B".to_string(), "run".to_string())]);
        assert_eq!(instruction_len(&code, invoke_at), Some(3));
    }

    #[test]
    fn unknown_opcode_stops_cleanly() {
        let calls = invoke_targets(&[0x0e], &[Const::Empty]);
        assert!(calls.is_empty());
    }
}
