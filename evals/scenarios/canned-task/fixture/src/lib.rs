pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[test]
fn adds() {
    assert_eq!(add(2, 2), 4);
}
