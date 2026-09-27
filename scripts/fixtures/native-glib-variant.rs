// Deliberately separate from the unmodified upstream crate. Compile optimized
// against Cargo's exact patched GLib artifact; no GTK window or model needed.
use glib::{variant::ToVariant, Variant};

fn strings(values: &[&str]) -> Variant {
    values.to_variant()
}

#[test]
fn forward_and_backward_preserve_empty_and_unicode_strings() {
    let values = ["", "ASCII", "λ雪🦀", "last"];
    let variant = strings(&values);
    assert_eq!(
        variant.array_iter_str().unwrap().collect::<Vec<_>>(),
        values
    );
    assert_eq!(
        variant.array_iter_str().unwrap().rev().collect::<Vec<_>>(),
        values.into_iter().rev().collect::<Vec<_>>()
    );
}

#[test]
fn next_and_next_back_share_one_bounded_cursor() {
    let variant = strings(&["first", "middle", "last"]);
    let mut iter = variant.array_iter_str().unwrap();
    assert_eq!(iter.size_hint(), (3, Some(3)));
    assert_eq!(iter.next(), Some("first"));
    assert_eq!(iter.next_back(), Some("last"));
    assert_eq!(iter.len(), 1);
    assert_eq!(iter.next_back(), Some("middle"));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
    assert_eq!(iter.len(), 0);
}

#[test]
fn nth_nth_back_and_last_use_the_correct_out_pointer() {
    let variant = strings(&["0", "雪", "2", "3", "🦀", "5"]);
    let mut iter = variant.array_iter_str().unwrap();
    assert_eq!(iter.nth(1), Some("雪"));
    assert_eq!(iter.nth_back(1), Some("🦀"));
    assert_eq!(iter.len(), 2);
    assert_eq!(iter.last(), Some("3"));
    assert_eq!(variant.array_iter_str().unwrap().last(), Some("5"));
}

#[test]
fn empty_arrays_and_exhausted_cursors_are_fused() {
    let variant = strings(&[]);
    let mut iter = variant.array_iter_str().unwrap();
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
    assert_eq!(iter.nth(0), None);
    assert_eq!(iter.nth_back(0), None);
    assert_eq!(iter.last(), None);
    let one = strings(&[""]);
    let mut iter = one.array_iter_str().unwrap();
    assert_eq!(iter.next(), Some(""));
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next_back(), None);
}

#[test]
fn huge_skips_exhaust_without_wrapping() {
    let variant = strings(&["a", "b"]);
    let mut forward = variant.array_iter_str().unwrap();
    assert_eq!(forward.next(), Some("a"));
    assert_eq!(forward.nth(usize::MAX), None);
    assert_eq!(forward.next_back(), None);
    let mut backward = variant.array_iter_str().unwrap();
    assert_eq!(backward.nth_back(usize::MAX), None);
    assert_eq!(backward.next(), None);
}

#[test]
fn non_string_array_is_refused() {
    assert!([1u32, 2u32].to_variant().array_iter_str().is_err());
}
