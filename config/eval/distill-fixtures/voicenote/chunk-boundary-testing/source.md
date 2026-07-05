# Voice note transcript (synthetic, non-personal)

[00:00] Note to self about the chunking code. The boundary finder is what
decides where a long transcript gets split, and I think our tests are too weak.

[00:14] Right now we only test that chunking a big blob produces more than one
chunk. That's not enough. The real risk is that a chunk boundary lands in the
middle of a sentence, or worse, in the middle of a multi-byte character, which
would panic.

[00:38] So I want three specific tests. One: feed a transcript with a very long
run of text and no natural break, and assert we still split near the target
size instead of blowing past it. Two: feed text with emoji and accented
characters right at the boundary offset and assert no panic and no replacement
characters. Three: assert the concatenation of all chunks equals the original
input, byte for byte, so we never drop or duplicate content.

[01:10] The third one is the important invariant — round-trip equality. If
chunks don't reassemble to the original, we're silently losing text somewhere,
and that's exactly the class of bug that's hard to notice in production.

[01:32] I should also add a property test if it's cheap: random inputs, chunk,
reassemble, assert equality. But the three concrete tests come first.
