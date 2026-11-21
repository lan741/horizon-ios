use super::*;
use crate::shader;

const VERT: &str = r#"#version 330 core
layout(location = 0) in vec3 position;
layout(location = 1) in vec2 uv;
out vec2 v_uv;
out vec3 v_normal;
uniform mat4 mvp;
uniform float time;
void main() {
    vec3 p = position + vec3(0.0, sin(time) * 0.01, 0.0);
    v_uv = uv;
    v_normal = vec3(0.0, 1.0, 0.0);
    gl_Position = mvp * vec4(p, 1.0);
}
"#;

const FRAG: &str = r#"#version 330 core
in vec2 v_uv;
in vec3 v_normal;
out vec4 colour;
uniform sampler2D textureArray;
void main() {
    vec4 base = texture(textureArray, v_uv);
    float light = clamp(dot(normalize(v_normal), vec3(0.0, 1.0, 0.0)), 0.0, 1.0);
    if (light < 0.05) {
        discard;
    }
    colour = vec4(base.rgb * light, 1.0);
}
"#;

fn pack() -> shader::ShaderPack {
    shader::compile(
        "demo",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, FRAG.to_string()),
        ],
    )
}

fn translate_demo() -> TranslatedPack {
    translate(
        &pack(),
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, FRAG.to_string()),
        ],
    )
    .expect("the demo pack is inside the subset")
}

#[test]
fn emits_a_complete_translation_unit() {
    let out = translate_demo();
    assert!(out.msl.contains("#include <metal_stdlib>"));
    assert!(out.msl.contains("struct HzUniforms"));
    assert!(out.msl.contains("struct HzVaryings"));
    assert!(out.msl.contains("struct HzVertexIn"));
    assert!(out.msl.contains("vertex HzVaryings hz_vertex"));
    assert!(out.msl.contains("fragment float4 hz_fragment"));
    assert_eq!(out.vertex.entry, "hz_vertex");
    assert_eq!(out.fragment.entry, "hz_fragment");
}

#[test]
fn attributes_bind_to_engine_slots_by_name() {
    let out = translate_demo();
    assert!(out.msl.contains("float3 position [[attribute(0)]]"));
    assert!(out.msl.contains("float2 uv [[attribute(1)]]"));
}

#[test]
fn attribute_slots_do_not_follow_declaration_order() {
    // `uv` declared first must still bind to slot 1: the buffer layout is the
    // engine's, and reading the wrong offset renders garbage that still runs.
    let vert = r#"#version 330
layout(location = 0) in vec2 uv;
layout(location = 1) in vec3 position;
out vec2 v_uv;
void main() {
    v_uv = uv;
    gl_Position = vec4(position, 1.0);
}
"#;
    let frag =
        "#version 330\nin vec2 v_uv;\nout vec4 c;\nvoid main() { c = vec4(v_uv, 0.0, 1.0); }\n";
    let p = shader::compile(
        "swapped",
        &[
            (StageKind::Vertex, vert.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let out = translate(
        &p,
        &[
            (StageKind::Vertex, vert.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .unwrap();
    assert!(
        out.msl.contains("float2 uv [[attribute(1)]]"),
        "{}",
        out.msl
    );
    assert!(
        out.msl.contains("float3 position [[attribute(0)]]"),
        "{}",
        out.msl
    );
}

#[test]
fn an_unknown_vertex_input_is_refused() {
    let vert = r#"#version 330
layout(location = 0) in vec3 position;
in vec4 tangent;
out vec2 v_uv;
void main() {
    v_uv = vec2(0.0);
    gl_Position = vec4(position + tangent.xyz, 1.0);
}
"#;
    let frag = "#version 330\nin vec2 v_uv;\nout vec4 c;\nvoid main() { c = vec4(1.0); }\n";
    let p = shader::compile(
        "tangent",
        &[
            (StageKind::Vertex, vert.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let err = translate(
        &p,
        &[
            (StageKind::Vertex, vert.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .expect_err("the engine has no tangent attribute");
    assert!(
        err.iter()
            .any(|d| d.message.contains("unknown vertex input")),
        "{err:?}"
    );
}

#[test]
fn varyings_carry_the_vertex_outputs() {
    let out = translate_demo();
    assert!(out.msl.contains("float4 position [[position]]"));
    assert!(out.msl.contains("float2 v_uv [[user(locn1)]]"));
    assert!(out.msl.contains("float3 v_normal [[user(locn2)]]"));
}

#[test]
fn uniforms_are_declared_in_the_engine_block() {
    let out = translate_demo();
    assert!(out.msl.contains("float4x4 mvp;"));
    assert!(out.msl.contains("float time;"));
    assert!(out.msl.contains("float4 fogColor;"));
}

#[test]
fn built_in_outputs_are_rewritten() {
    let out = translate_demo();
    assert!(
        out.vertex.body.contains("out.position ="),
        "{}",
        out.vertex.body
    );
    assert!(!out.vertex.body.contains("gl_Position"));
    assert!(
        out.fragment.body.contains("hz_colour ="),
        "{}",
        out.fragment.body
    );
    assert!(!out.fragment.body.contains("gl_FragColor"));
}

#[test]
fn texture_sampling_becomes_a_metal_sample() {
    let out = translate_demo();
    assert!(
        out.fragment
            .body
            .contains("hz_tex.sample(hz_smp, in.v_uv, 0)"),
        "{}",
        out.fragment.body
    );
    assert!(
        out.notes.iter().any(|n| n.contains("slice 0")),
        "the slice limitation must be reported: {:?}",
        out.notes
    );
}

#[test]
fn builtins_are_renamed_where_metal_differs() {
    let frag = r#"#version 330
in vec2 v_uv;
out vec4 colour;
uniform sampler2D textureArray;
void main() {
    float a = fract(v_uv.x);
    float b = mod(a, 0.5);
    float c = inversesqrt(max(b, 0.001));
    float d = abs(c - 1.0);
    colour = vec4(d, a, b, 1.0);
}
"#;
    let p = shader::compile(
        "builtins",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let out = translate(
        &p,
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .unwrap();
    assert!(
        out.fragment.body.contains("fract("),
        "{}",
        out.fragment.body
    );
    assert!(out.fragment.body.contains("fmod("), "{}", out.fragment.body);
    assert!(
        out.fragment.body.contains("rsqrt("),
        "{}",
        out.fragment.body
    );
    assert!(out.fragment.body.contains("fabs("), "{}", out.fragment.body);
}

#[test]
fn if_else_and_discard_translate() {
    let frag = r#"#version 330
in vec2 v_uv;
out vec4 colour;
void main() {
    if (v_uv.x > 0.5) {
        colour = vec4(1.0);
    } else {
        discard;
    }
}
"#;
    let p = shader::compile(
        "branch",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let out = translate(
        &p,
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .unwrap();
    assert!(
        out.fragment.body.contains("if ((in.v_uv.x > 0.5))"),
        "{}",
        out.fragment.body
    );
    assert!(
        out.fragment.body.contains("else {"),
        "{}",
        out.fragment.body
    );
    assert!(out.fragment.body.contains("discard_fragment();"));
}

#[test]
fn a_loop_is_refused_with_a_line_number() {
    let frag = r#"#version 330
in vec2 v_uv;
out vec4 colour;
void main() {
    float sum = 0.0;
    for (int i = 0; i < 4; i = i + 1) {
        sum = sum + 1.0;
    }
    colour = vec4(sum);
}
"#;
    let p = shader::compile(
        "loop",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let err = translate(
        &p,
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .expect_err("loops are outside the subset");
    assert!(
        err.iter()
            .any(|d| d.stage == StageKind::Fragment && d.message.contains("loops")),
        "{err:?}"
    );
    // the diagnostic carries the line so the pack author can act on it
    assert!(err.iter().any(|d| d.line >= 6), "{err:?}");
}

#[test]
fn user_functions_are_refused() {
    let frag = r#"#version 330
in vec2 v_uv;
out vec4 colour;
float helper(float x) { return x * 2.0; }
void main() {
    colour = vec4(helper(v_uv.x));
}
"#;
    let p = shader::compile(
        "userfn",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let err = translate(
        &p,
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .expect_err("user functions are not in the subset");
    assert!(
        err.iter()
            .any(|d| d.message.contains("not a supported built-in")),
        "{err:?}"
    );
}

#[test]
fn ternary_array_and_preprocessor_are_refused() {
    for (snippet, needle) in [
        ("float a = v_uv.x > 0.5 ? 1.0 : 0.0;", "ternary"),
        ("float a = v_uv[0];", "array indexing"),
        ("#define TWO 2.0\n    float a = TWO;", "preprocessor"),
    ] {
        let frag = format!(
            "#version 330\nin vec2 v_uv;\nout vec4 colour;\nvoid main() {{\n    {snippet}\n    colour = vec4(1.0);\n}}\n"
        );
        let p = shader::compile(
            "unsupported",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, frag.clone()),
            ],
        );
        let err = translate(
            &p,
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, frag.clone()),
            ],
        )
        .expect_err("should refuse");
        assert!(
            err.iter()
                .any(|d| d.message.to_lowercase().contains(&needle.to_lowercase())),
            "expected {needle} for {snippet}, got {err:?}"
        );
    }
}

#[test]
fn missing_stages_are_reported() {
    let p = shader::compile("only-frag", &[(StageKind::Fragment, FRAG.to_string())]);
    let err = translate(&p, &[(StageKind::Fragment, FRAG.to_string())])
        .expect_err("needs a vertex stage too");
    assert!(err.iter().any(|d| d.message.contains("both a vertex")));
}

#[test]
fn numeric_literals_become_floats() {
    let frag = r#"#version 330
in vec2 v_uv;
out vec4 colour;
void main() {
    colour = vec4(1, 0, 0, 1);
}
"#;
    let p = shader::compile(
        "ints",
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    );
    let out = translate(
        &p,
        &[
            (StageKind::Vertex, VERT.to_string()),
            (StageKind::Fragment, frag.to_string()),
        ],
    )
    .unwrap();
    assert!(
        out.fragment.body.contains("float4(1.0, 0.0, 0.0, 1.0)"),
        "{}",
        out.fragment.body
    );
}
