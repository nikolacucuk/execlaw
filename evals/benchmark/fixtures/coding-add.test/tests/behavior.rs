use benchmark_coding_fixture::checked_add;

#[test]
fn adds_values_without_overflow() {
    assert_eq!(checked_add(20, 22), Some(42));
}

#[test]
fn reports_overflow() {
    assert_eq!(checked_add(i32::MAX, 1), None);
}
