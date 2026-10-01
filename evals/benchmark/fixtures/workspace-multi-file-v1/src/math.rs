pub fn sum(values: &[i32]) -> i32 {
    values
        .iter()
        .fold(0, |total, value| total.wrapping_add(*value))
}
