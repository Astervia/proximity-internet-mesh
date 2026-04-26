use super::super::*;

#[test]
fn parse_stats_str_extracts_key_value_pairs() {
    let input = "peers=3\nroutes=5\npackets_forwarded=100\nbytes_forwarded=51200\n";
    let pairs = parse_stats_str(input);
    assert_eq!(pairs.len(), 4);
    assert_eq!(pairs[0], ("peers".to_string(), "3".to_string()));
    assert_eq!(pairs[1], ("routes".to_string(), "5".to_string()));
    assert_eq!(
        pairs[2],
        ("packets_forwarded".to_string(), "100".to_string())
    );
    assert_eq!(
        pairs[3],
        ("bytes_forwarded".to_string(), "51200".to_string())
    );
}

#[test]
fn parse_stats_str_skips_malformed_lines() {
    let input = "peers=3\nnot-a-pair\nbytes_forwarded=512\n";
    let pairs = parse_stats_str(input);
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0].0, "peers");
    assert_eq!(pairs[1].0, "bytes_forwarded");
}

#[test]
fn parse_stats_str_empty_input() {
    let pairs = parse_stats_str("");
    assert!(pairs.is_empty());
}
