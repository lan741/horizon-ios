//! A valid, minimal `.class` used by tests, the CLI and the bench harness.
//!
//! Hand-built so it needs no JVM at build time: one class, one static method
//! whose body is `invokestatic <callee>; return`.

/// Build a minimal but valid class file by hand: a class with one static
/// method containing `invokestatic` on a known target.
pub fn tiny_class(name: &str, callee_owner: &str, callee: &str) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"\xca\xfe\xba\xbe");
    out.extend_from_slice(&[0, 0, 0, 65]); // minor 0, major 65 (Java 21)

    let mut pool: Vec<Vec<u8>> = Vec::new(); // index 1-based via +1
    let add_utf8 = |pool: &mut Vec<Vec<u8>>, s: &str| -> u16 {
        let mut e = vec![1u8];
        e.extend_from_slice(&(s.len() as u16).to_be_bytes());
        e.extend_from_slice(s.as_bytes());
        pool.push(e);
        pool.len() as u16
    };

    let this_name = add_utf8(&mut pool, name);
    let super_name = add_utf8(&mut pool, "java/lang/Object");
    let method_name = add_utf8(&mut pool, "run");
    let method_desc = add_utf8(&mut pool, "()V");
    let code_name = add_utf8(&mut pool, "Code");
    let callee_owner_s = add_utf8(&mut pool, callee_owner);
    let callee_name_s = add_utf8(&mut pool, callee);
    let callee_desc = add_utf8(&mut pool, "()V");

    // Class entries
    let class_entry = |pool: &mut Vec<Vec<u8>>, idx: u16| -> u16 {
        let mut e = vec![7u8];
        e.extend_from_slice(&idx.to_be_bytes());
        pool.push(e);
        pool.len() as u16
    };
    let this_class = class_entry(&mut pool, this_name);
    let super_class = class_entry(&mut pool, super_name);
    let callee_class = class_entry(&mut pool, callee_owner_s);

    // NameAndType + Methodref
    let mut nat = vec![12u8];
    nat.extend_from_slice(&callee_name_s.to_be_bytes());
    nat.extend_from_slice(&callee_desc.to_be_bytes());
    pool.push(nat);
    let nat_idx = pool.len() as u16;
    let mut mref = vec![10u8];
    mref.extend_from_slice(&callee_class.to_be_bytes());
    mref.extend_from_slice(&nat_idx.to_be_bytes());
    pool.push(mref);
    let mref_idx = pool.len() as u16;

    out.extend_from_slice(&((pool.len() + 1) as u16).to_be_bytes());
    for e in &pool {
        out.extend_from_slice(e);
    }

    out.extend_from_slice(&0x0021u16.to_be_bytes()); // public super
    out.extend_from_slice(&this_class.to_be_bytes());
    out.extend_from_slice(&super_class.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // interfaces

    out.extend_from_slice(&0u16.to_be_bytes()); // fields

    out.extend_from_slice(&1u16.to_be_bytes()); // one method
    out.extend_from_slice(&0x0009u16.to_be_bytes()); // public static
    out.extend_from_slice(&method_name.to_be_bytes());
    out.extend_from_slice(&method_desc.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // attributes: Code
    out.extend_from_slice(&code_name.to_be_bytes());
    // Code body: invokestatic, return
    let code: Vec<u8> = {
        let mut c = vec![0xb8u8];
        c.extend_from_slice(&mref_idx.to_be_bytes());
        c.push(0xb1); // return
        c
    };
    let attr_len = 2 + 2 + 4 + code.len() + 2 + 2;
    out.extend_from_slice(&(attr_len as u32).to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // max_stack
    out.extend_from_slice(&0u16.to_be_bytes()); // max_locals
    out.extend_from_slice(&(code.len() as u32).to_be_bytes());
    out.extend_from_slice(&code);
    out.extend_from_slice(&0u16.to_be_bytes()); // exception table
    out.extend_from_slice(&0u16.to_be_bytes()); // nested attributes

    out.extend_from_slice(&0u16.to_be_bytes()); // class attributes
    out
}
