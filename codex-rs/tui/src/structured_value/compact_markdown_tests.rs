use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn body(data: &Value) -> String {
    format(
        data,
        Options {
            title: None,
            ..Default::default()
        },
    )
}

#[test]
fn scalar_empty_and_leaf_array_goldens_match_the_supplied_algorithm() {
    for (data, expected) in [
        (Value::Null, "- `null`\n"),
        (json!(false), "- `false`\n"),
        (json!(0), "- `0`\n"),
        (json!(""), "- `\"\"`\n"),
        (json!([]), "- `[]`\n"),
        (json!({}), "- `{}`\n"),
        (
            json!([null, false, 0, "", [], {}]),
            "- `null` · `false` · `0` · `\"\"` · `[]` · `{}`\n",
        ),
    ] {
        assert_eq!(body(&data), expected);
    }
    assert_eq!(
        format(&json!(true), Options::default()),
        "# Result\n\n- `true`\n"
    );
}

#[test]
fn arrays_matrices_and_nested_objects_keep_the_exact_branch_order() {
    assert_eq!(
        body(&json!([[1, "two"], [], [false, null]])),
        "1. `1` · two\n2. `[]`\n3. `false` · `null`\n"
    );
    assert_eq!(
        body(&json!([{"name":"Build","ok":true}, ["one", {"x":2}]])),
        "- **1** · **name:** Build · **ok:** `true`\n- **2**\n  - **1:** one\n  - **2** · **x:** `2`\n"
    );
    assert_eq!(
        body(&json!({"summary":"Ready","checks":[{"name":"Build","ok":true},{"id":"Only"},{}]})),
        "- **summary:** Ready\n\n## checks\n\n### 1 · name: Build\n\n- **ok:** `true`\n\n### 2 · id: Only\n\n### 3\n\n- `{}`\n"
    );
}

#[test]
fn section_identity_uses_the_first_short_string_without_losing_its_field() {
    let data =
        json!({"checks":[{"count":0,"first":"Alpha","second":"Beta","nested":{"name":"Child"}}]});
    let markdown = body(&data);
    assert_eq!(
        markdown,
        "## checks\n\n### 1 · first: Alpha\n\n- **count:** `0` · **second:** Beta\n- **nested** · **name:** Child\n"
    );
    assert_eq!(markdown.matches("Alpha").count(), 1);
    // The root-array branch deliberately does not create section identity headings.
    assert!(!body(&data["checks"]).contains("###"));
}

#[test]
fn markdown_newlines_and_control_bytes_are_literal_and_not_terminal_commands() {
    let data = json!({"":"", "a*b": "\\`*_{}[]<>|#!+~-\r\nnext\rlast\nend\u{001b}[31m"});
    let markdown = body(&data);
    assert!(markdown.contains("**`\"\"`:** `\"\"`"), "{markdown}");
    assert!(
        markdown.contains(
            r"**a\*b:** \\\`\*\_\{\}\[\]\<\>\|\#\!\+\~\- ⏎ next ⏎ last ⏎ end\\u001b\[31m"
        ),
        "{markdown}"
    );
    assert!(!markdown.contains('\u{001b}'));
    assert_eq!(
        format(
            &json!(null),
            Options {
                title: Some("A *title*"),
                ..Default::default()
            }
        ),
        "# A \\*title\\*\n\n- `null`\n"
    );
}

#[test]
fn packing_and_short_values_measure_utf16_source_not_display_cells() {
    assert_eq!(
        format(
            &json!({"a":"🙂","b":"x"}),
            Options {
                title: None,
                width: 19,
                ..Default::default()
            }
        ),
        "- **a:** 🙂\n- **b:** x\n"
    );
    let long = "x".repeat(49);
    assert_eq!(
        body(&json!({"record":{"first":"x".repeat(48),"second":long}})),
        format!(
            "## record\n\n- **first:** {}\n- **second:** {}\n",
            "x".repeat(48),
            "x".repeat(49)
        )
    );
    assert_eq!(
        format(
            &json!(["a", "b", "c"]),
            Options {
                title: None,
                width: 5,
                ..Default::default()
            }
        ),
        "- a · b\n- c\n"
    );
}

#[test]
fn object_entries_use_javascript_array_index_order_then_insertion_order() {
    let mut fields = Map::new();
    for key in ["10", "2", "01", "name", "4294967295", "0"] {
        fields.insert(key.into(), json!(1));
    }
    let markdown = format(
        &Value::Object(fields),
        Options {
            title: None,
            width: 1,
            ..Default::default()
        },
    );
    assert_eq!(
        markdown,
        "- **0:** `1`\n- **2:** `1`\n- **10:** `1`\n- **01:** `1`\n- **name:** `1`\n- **4294967295:** `1`\n"
    );
}

#[test]
fn depth_limit_flattens_every_leaf_with_relative_rfc6901_paths() {
    assert_eq!(
        format(
            &json!({"a":false}),
            Options {
                title: None,
                max_indent_depth: 0,
                ..Default::default()
            }
        ),
        "- **/a:** `false`\n"
    );
    let data = json!({"section":{"a":{"~/":{"": [false, {}, []]}}}});
    assert_eq!(
        format(
            &data,
            Options {
                title: None,
                max_indent_depth: 0,
                ..Default::default()
            }
        ),
        "## section\n\n- **/a/\\~0\\~1//0:** `false`\n- **/a/\\~0\\~1//1:** `{}`\n- **/a/\\~0\\~1//2:** `[]`\n"
    );
    let nested = json!([{"a":{"b":{"c":{"d":{"e":"kept"}}}}}]);
    assert_eq!(
        body(&nested),
        "- **1**\n  - **a**\n    - **b**\n      - **c** (relative JSON Pointer paths)\n        - **/d/e:** kept\n"
    );
}

#[test]
fn deep_and_wide_values_are_iterative_and_keep_every_terminal_value() {
    let mut deep = json!("deep-leaf");
    for _ in 0..2048 {
        deep = Value::Array(vec![deep]);
    }
    let markdown = body(&deep);
    assert_eq!(markdown.matches("deep\\-leaf").count(), 1);
    assert!(!markdown.contains("continues") && !markdown.contains('…'));
    // Avoid exercising serde_json::Value's recursive destructor in this traversal test.
    while let Value::Array(values) = &mut deep {
        deep = values.pop().unwrap_or(Value::Null);
    }

    let wide = Value::Array(
        (0..4096)
            .map(|index| json!(format!("value{index}END")))
            .collect(),
    );
    let markdown = body(&wide);
    let retained = markdown
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.starts_with("value"))
        .collect::<Vec<_>>();
    let expected = (0..4096)
        .map(|index| format!("value{index}END"))
        .collect::<Vec<_>>();
    assert_eq!(retained, expected);
    assert!(markdown.ends_with("value4095END\n"));
}

#[test]
fn exact_json_numbers_are_not_rounded_through_javascript_floats() {
    assert_eq!(
        body(&json!([9_007_199_254_740_993_u64, u64::MAX])),
        "- `9007199254740993` · `18446744073709551615`\n"
    );
}
