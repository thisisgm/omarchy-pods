//! The document's tables against `INTROSPECTED`, which the snapshot test pins to the interface.

use std::collections::BTreeMap;

use pods_core::command::CommandError;
use pods_core::settings;

use super::{API_DOCUMENT, INTROSPECTED};

// Sample input: `| `SetOneBudAnc` | `b` | `one_bud_anc` | |`, four cells, the last empty.
fn table(heading: &str) -> Vec<Vec<String>> {
    let (_, section) = API_DOCUMENT
        .split_once(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("docs/dbus-api.md has no `{heading}` section"));
    section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|row| {
            row.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().replace('`', ""))
                .collect()
        })
        .collect()
}

// Sample input: ` name="mode" type="s" direction="in"/>` with `type` gives `s`.
fn attribute<'a>(element: &'a str, name: &str) -> &'a str {
    let tag = element.split('>').next().unwrap_or_default();
    tag.split_once(&format!(" {name}=\""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or("", |(value, _)| value)
}

fn introspected_properties() -> BTreeMap<String, String> {
    INTROSPECTED
        .split("<property")
        .skip(1)
        .map(|element| {
            let name = attribute(element, "name").to_owned();
            (name, attribute(element, "type").to_owned())
        })
        .collect()
}

fn introspected_methods() -> BTreeMap<String, (String, String)> {
    INTROSPECTED
        .split("<method")
        .skip(1)
        .map(|element| {
            let (body, _) = element.split_once("</method>").unwrap();
            let mut input = String::new();
            let mut output = String::new();
            for argument in body.split("<arg").skip(1) {
                match attribute(argument, "direction") {
                    "in" => input.push_str(attribute(argument, "type")),
                    "out" => output.push_str(attribute(argument, "type")),
                    other => panic!("an argument of direction {other:?}: {argument}"),
                }
            }
            (attribute(element, "name").to_owned(), (input, output))
        })
        .collect()
}

// Sample input: `Each method returns `s`: `sent` when the pods have the command` gives `s`.
fn documented_reply() -> String {
    let line = API_DOCUMENT
        .lines()
        .find(|line| line.starts_with("Each method returns `"))
        .expect("docs/dbus-api.md has no line naming what a method returns");
    line.split('`').nth(1).unwrap_or_default().to_owned()
}

#[test]
fn the_property_table_is_the_interfaces_properties_with_their_types() {
    let rows = table("## Properties");
    let documented: BTreeMap<String, String> = rows
        .iter()
        .map(|row| match row.as_slice() {
            [name, signature, _value] => (name.clone(), signature.clone()),
            _ => panic!("a property row without three cells: {row:?}"),
        })
        .collect();
    assert_eq!(documented.len(), rows.len(), "a property has two rows");
    assert_eq!(documented, introspected_properties());
}

#[test]
fn the_method_table_is_the_interfaces_methods_with_their_signatures() {
    let reply = documented_reply();
    let rows = table("## Methods");
    let documented: BTreeMap<String, (String, String)> = rows
        .iter()
        .map(|row| match row.as_slice() {
            [name, argument, _key, _accepts] => {
                let input = if argument == "none" { "" } else { argument };
                (name.clone(), (input.to_owned(), reply.clone()))
            }
            _ => panic!("a method row without four cells: {row:?}"),
        })
        .collect();
    assert_eq!(documented.len(), rows.len(), "a method has two rows");
    assert_eq!(documented, introspected_methods());
}

#[test]
fn every_set_method_names_a_setting_key_and_no_other_method_does() {
    for row in table("## Methods") {
        let [name, _argument, key, _accepts] = row.as_slice() else {
            panic!("a method row without four cells: {row:?}");
        };
        assert_eq!(name.starts_with("Set"), !key.is_empty(), "{row:?}");
        if !key.is_empty() {
            let refusal = settings::setting(key, "").err();
            assert_ne!(refusal, Some(CommandError::UnknownKey), "{row:?}");
        }
    }
}
