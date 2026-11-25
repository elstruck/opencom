use opencom::model::ShowConfig;

#[test]
fn example_show_parses() {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/theatre-show.json")).unwrap();
    let cfg: ShowConfig = serde_json::from_str(&raw).unwrap();
    assert_eq!(cfg.channels.len(), 4);
    assert_eq!(cfg.automations.len(), 3);
}
