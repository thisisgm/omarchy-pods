//! Support tests: the one word that holds the run's arrival or none.

use super::Arrival;

#[test]
fn an_arrival_is_never_read_back_as_none() {
    let word = Arrival::none();
    assert_eq!(word.load(), None);
    for ns in [i64::MAX, 0, -1, i64::MIN + 1] {
        word.store(Some(ns));
        assert_eq!(word.load(), Some(ns));
    }
    // The one value the word keeps for none is still an arrival when it is stored: one ns later.
    word.store(Some(i64::MIN));
    assert_eq!(word.load(), Some(i64::MIN + 1));
    word.store(None);
    assert_eq!(word.load(), None);
}
