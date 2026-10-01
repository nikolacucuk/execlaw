use multi_file_repair::checked_sum;

#[test]
fn checked_sum_preserves_valid_values() {
    assert_eq!(checked_sum(&[4, -2, 9]), Some(11));
}

#[test]
fn checked_sum_rejects_overflow() {
    assert_eq!(checked_sum(&[i32::MAX, 1]), None);
}
