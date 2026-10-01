mod math;

pub fn checked_sum(values: &[i32]) -> Option<i32> {
    Some(math::sum(values))
}
