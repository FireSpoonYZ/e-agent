//! Losslessly compressed text resources bundled into the shipping binary.
//!
//! These resources are large, immutable source snapshots. File-backed assets
//! are deterministically compressed by `build.rs`; inline JavaScript literals
//! use the const LZSS codec below. This module decodes both forms on demand.
//! Parse-only callers own a temporary `String`; repeatedly used resources are
//! retained by their owning `OnceLock`. Every caller receives the exact
//! original UTF-8 bytes.

// A 4 Ki-entry table keeps compile-time evaluation bounded while retaining
// nearly all of the compression benefit on the bundled JavaScript corpus.
// Larger tables made every target spend minutes initializing const state.
const LZSS_HASH_SLOTS: usize = 1 << 12;
const LZSS_MAX_DISTANCE: usize = 65_535;
const LZSS_MIN_MATCH: usize = 4;
const LZSS_MAX_MATCH: usize = LZSS_MIN_MATCH + 0x7f;
const LZSS_LITERAL_CHUNK: usize = 0x80;

const fn lzss_hash(input: &[u8], position: usize) -> usize {
    let mut sequence_bytes = [0; std::mem::size_of::<usize>()];
    sequence_bytes[0] = input[position];
    sequence_bytes[1] = input[position + 1];
    sequence_bytes[2] = input[position + 2];
    let sequence = usize::from_le_bytes(sequence_bytes);
    (sequence.wrapping_mul(2_654_435_761) >> 16) & (LZSS_HASH_SLOTS - 1)
}

const fn lzss_match(
    input: &[u8],
    positions: &mut [usize; LZSS_HASH_SLOTS],
    position: usize,
) -> (usize, usize) {
    if position + LZSS_MIN_MATCH > input.len() {
        return (0, 0);
    }
    let hash = lzss_hash(input, position);
    let candidate = positions[hash];
    positions[hash] = position;
    if candidate == usize::MAX || candidate >= position || position - candidate > LZSS_MAX_DISTANCE
    {
        return (0, 0);
    }
    let available = input.len() - position;
    let limit = if available < LZSS_MAX_MATCH {
        available
    } else {
        LZSS_MAX_MATCH
    };
    let mut length = 0;
    while length < limit && input[candidate + length] == input[position + length] {
        length += 1;
    }
    if length < LZSS_MIN_MATCH {
        (0, 0)
    } else {
        (length, position - candidate)
    }
}

const fn lzss_record_match_positions(
    input: &[u8],
    positions: &mut [usize; LZSS_HASH_SLOTS],
    position: usize,
    match_length: usize,
) {
    let mut cursor = position + 1;
    let end = position + match_length;
    while cursor < end {
        if cursor + LZSS_MIN_MATCH <= input.len() {
            positions[lzss_hash(input, cursor)] = cursor;
        }
        cursor += 1;
    }
}

const fn lzss_literal_encoded_len(length: usize) -> usize {
    length + length.div_ceil(LZSS_LITERAL_CHUNK)
}

/// Return the size of the deterministic LZSS representation used for large
/// JavaScript literals. This is evaluated by rustc; the raw literal is not
/// needed by the release binary.
#[expect(
    clippy::large_stack_arrays,
    reason = "the hash table exists only during compile-time evaluation of embedded literals"
)]
pub const fn lzss_compressed_len(input: &[u8]) -> usize {
    let mut positions = [usize::MAX; LZSS_HASH_SLOTS];
    let mut position = 0;
    let mut literal_start = 0;
    let mut encoded_len = 0;
    while position < input.len() {
        let (match_length, _) = lzss_match(input, &mut positions, position);
        if match_length == 0 {
            position += 1;
            continue;
        }
        encoded_len += lzss_literal_encoded_len(position - literal_start) + 3;
        lzss_record_match_positions(input, &mut positions, position, match_length);
        position += match_length;
        literal_start = position;
    }
    encoded_len + lzss_literal_encoded_len(input.len() - literal_start)
}

/// Encode a text literal into the deterministic LZSS representation.
#[expect(
    clippy::large_stack_arrays,
    reason = "the hash table exists only during compile-time evaluation of embedded literals"
)]
pub const fn lzss_compress<const OUTPUT_LEN: usize>(input: &[u8]) -> [u8; OUTPUT_LEN] {
    let mut positions = [usize::MAX; LZSS_HASH_SLOTS];
    let mut output = [0; OUTPUT_LEN];
    let mut output_position = 0;
    let mut position = 0;
    let mut literal_start = 0;
    while position < input.len() {
        let (match_length, match_distance) = lzss_match(input, &mut positions, position);
        if match_length == 0 {
            position += 1;
            continue;
        }
        let mut literal_position = literal_start;
        while literal_position < position {
            let remaining = position - literal_position;
            let chunk_len = if remaining < LZSS_LITERAL_CHUNK {
                remaining
            } else {
                LZSS_LITERAL_CHUNK
            };
            output[output_position] = (chunk_len - 1).to_le_bytes()[0];
            output_position += 1;
            let literal_end = literal_position + chunk_len;
            while literal_position < literal_end {
                output[output_position] = input[literal_position];
                output_position += 1;
                literal_position += 1;
            }
        }
        output[output_position] = 0x80 | (match_length - LZSS_MIN_MATCH).to_le_bytes()[0];
        let match_distance_bytes = match_distance.to_le_bytes();
        output[output_position + 1] = match_distance_bytes[0];
        output[output_position + 2] = match_distance_bytes[1];
        output_position += 3;
        lzss_record_match_positions(input, &mut positions, position, match_length);
        position += match_length;
        literal_start = position;
    }
    let mut literal_position = literal_start;
    while literal_position < input.len() {
        let remaining = input.len() - literal_position;
        let chunk_len = if remaining < LZSS_LITERAL_CHUNK {
            remaining
        } else {
            LZSS_LITERAL_CHUNK
        };
        output[output_position] = (chunk_len - 1).to_le_bytes()[0];
        output_position += 1;
        let literal_end = literal_position + chunk_len;
        while literal_position < literal_end {
            output[output_position] = input[literal_position];
            output_position += 1;
            literal_position += 1;
        }
    }
    assert!(
        output_position == OUTPUT_LEN,
        "LZSS encoded length mismatch"
    );
    output
}

/// Decode a compile-time LZSS literal and fail closed on malformed metadata.
pub fn lzss_decompress(input: &[u8], expected_len: usize) -> Result<String, String> {
    let mut output = Vec::with_capacity(expected_len);
    let mut position = 0;
    while position < input.len() {
        let header = input[position];
        position += 1;
        if header & 0x80 == 0 {
            let length = usize::from(header) + 1;
            let end = position
                .checked_add(length)
                .filter(|end| *end <= input.len())
                .ok_or_else(|| "truncated LZSS literal run".to_string())?;
            output
                .len()
                .checked_add(length)
                .filter(|decoded_len| *decoded_len <= expected_len)
                .ok_or_else(|| "LZSS output exceeds declared length".to_string())?;
            output.extend_from_slice(&input[position..end]);
            position = end;
        } else {
            let distance_end = position
                .checked_add(2)
                .filter(|end| *end <= input.len())
                .ok_or_else(|| "truncated LZSS match distance".to_string())?;
            let distance = usize::from(u16::from_le_bytes([input[position], input[position + 1]]));
            position = distance_end;
            if distance == 0 || distance > output.len() {
                return Err("invalid LZSS match distance".to_string());
            }
            let length = usize::from(header & 0x7f) + LZSS_MIN_MATCH;
            output
                .len()
                .checked_add(length)
                .filter(|decoded_len| *decoded_len <= expected_len)
                .ok_or_else(|| "LZSS output exceeds declared length".to_string())?;
            for _ in 0..length {
                output.push(output[output.len() - distance]);
            }
        }
    }
    if output.len() != expected_len {
        return Err(format!(
            "LZSS output length mismatch: expected {expected_len}, decoded {}",
            output.len()
        ));
    }
    String::from_utf8(output).map_err(|error| format!("LZSS output is not UTF-8: {error}"))
}
