pub(crate) mod algs {
    use core::cmp::Ordering;

    /// Sorts `items` by `compare`, keeping equal items in their original order.
    ///
    /// An insertion sort, so quadratic: for sequences known to be short, as
    /// HarfBuzz uses `hb_stable_sort`.
    pub(crate) fn stable_sort_by<T: Copy>(
        items: &mut [T],
        mut compare: impl FnMut(&T, &T) -> Ordering,
    ) {
        for i in 1..items.len() {
            let item = items[i];
            let mut j = i;
            while j > 0 && compare(&items[j - 1], &item).is_gt() {
                items[j] = items[j - 1];
                j -= 1;
            }
            items[j] = item;
        }
    }

    /// Sorts `items` by `compare`, without allocating and without keeping equal
    /// items in their original order.
    ///
    /// A heap sort compiles to far less code per item type than the standard
    /// library's sorts, and the sorts done while building a plan are small.
    pub(crate) fn sort_unstable_by<T>(
        items: &mut [T],
        mut compare: impl FnMut(&T, &T) -> Ordering,
    ) {
        let len = items.len();
        for root in (0..len / 2).rev() {
            sift_down(items, root, len, &mut compare);
        }
        for end in (1..len).rev() {
            items.swap(0, end);
            sift_down(items, 0, end, &mut compare);
        }
    }

    fn sift_down<T>(
        items: &mut [T],
        mut root: usize,
        end: usize,
        compare: &mut impl FnMut(&T, &T) -> Ordering,
    ) {
        loop {
            let mut child = 2 * root + 1;
            if child >= end {
                break;
            }
            if child + 1 < end && compare(&items[child], &items[child + 1]).is_lt() {
                child += 1;
            }
            if !compare(&items[root], &items[child]).is_lt() {
                break;
            }
            items.swap(root, child);
            root = child;
        }
    }

    // FLAG macro in harfbuzz.
    #[inline]
    pub const fn rb_flag(x: u32) -> u32 {
        1 << x
    }

    // FLAG_UNSAFE macro in harfbuzz.
    #[inline]
    pub fn rb_flag_unsafe(x: u32) -> u32 {
        if x < 32 {
            1 << x
        } else {
            0
        }
    }

    // FLAG64 macro in harfbuzz.
    #[inline]
    pub const fn rb_flag64(x: u32) -> u64 {
        1 << x as u64
    }

    // FLAG64_UNSAFE macro in harfbuzz.
    #[inline]
    pub fn rb_flag64_unsafe(x: u32) -> u64 {
        if x < 64 {
            1 << (x as u64)
        } else {
            0
        }
    }

    /* Encodes three unsigned integers in one 64-bit number.  If the inputs have more than 21 bits,
     * values will be truncated / overlap, and might not decode exactly. */
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_ENCODE3(x: u32, y: u32, z: u32) -> u64 {
        ((x as u64) << 42) | ((y as u64) << 21) | (z as u64)
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_1(v: u64) -> u32 {
        (v >> 42) as u32
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_2(v: u64) -> u32 {
        ((v >> 21) & 0x001F_FFFF) as u32
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_3(v: u64) -> u32 {
        (v & 0x001F_FFFF) as u32
    }

    /* Custom encoding used by hb-ucd. */
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_ENCODE3_11_7_14(x: u32, y: u32, z: u32) -> u32 {
        ((x & 0x07FF) << 21) | ((y & 0x007F) << 14) | (z & 0x3FFF)
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_11_7_14_1(v: u32) -> u32 {
        v >> 21
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_11_7_14_2(v: u32) -> u32 {
        ((v >> 14) & 0x007F) | 0x0300
    }
    #[allow(non_snake_case, dead_code)]
    pub const fn HB_CODEPOINT_DECODE3_11_7_14_3(v: u32) -> u32 {
        v & 0x3FFF
    }

    #[cfg(test)]
    mod tests {
        use super::{sort_unstable_by, stable_sort_by};
        use alloc::vec::Vec;

        #[test]
        fn heap_sort_matches_std_sort() {
            for len in 0..200 {
                let mut items: Vec<u32> = (0..len).map(|i| (i * 173 + 31) % 37).collect();
                let mut expected = items.clone();
                expected.sort_unstable();
                sort_unstable_by(&mut items, Ord::cmp);
                assert_eq!(items, expected);
            }
        }

        #[test]
        fn insertion_sort_is_stable() {
            for len in 0..64 {
                let mut items: Vec<(u32, u32)> = (0..len).map(|i| ((i * 7 + 3) % 5, i)).collect();
                let mut expected = items.clone();
                expected.sort_by_key(|item| item.0);
                stable_sort_by(&mut items, |a, b| a.0.cmp(&b.0));
                assert_eq!(items, expected);
            }
        }
    }
}

pub(crate) mod budget {
    //! Limits on work and allocations while deriving cached OpenType layout data.
    //!
    //! These are independent of the buffer's shaping limits. They only run while
    //! building caches, including lazy builds shared across shaping calls. Once an
    //! allowance is insufficient, the affected lookup is discarded and stays
    //! skipped. Beyond the limits, the retained lookups can depend on use order.

    use core::mem::size_of;
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// A font's preprocessing allowance, shared by lazy cache initializers.
    ///
    /// Repeated visits to aliased data consume work again. Allocations consume
    /// their requested capacity, even if the resulting cache is later discarded.
    /// Neither allowance is replenished between lookup builds or shaping calls.
    #[derive(Debug)]
    pub(crate) struct Budget {
        work: AtomicUsize,
        bytes: AtomicUsize,
        optional_bytes: AtomicUsize,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Exhausted;

    impl Budget {
        // Calibrated against all lookups in 3716 Google Fonts and the HarfBuzz
        // performance/monster font collections. The highest measured work and
        // allocation use were below 5% of these allowances, including Gulzar's
        // 3625-subtable lookup. Ceilings also bound padding with unused data.
        pub fn for_table_bytes(bytes: usize) -> Self {
            Self::new(
                bytes.saturating_mul(256).clamp(4 << 20, 128 << 20),
                bytes.saturating_mul(256).clamp(4 << 20, 128 << 20),
            )
        }

        pub const fn new(work: usize, bytes: usize) -> Self {
            Self {
                work: AtomicUsize::new(work),
                bytes: AtomicUsize::new(bytes),
                optional_bytes: AtomicUsize::new(if bytes < 2 << 20 { bytes } else { 2 << 20 }),
            }
        }

        pub fn work(&self, visits: usize) -> Result<(), Exhausted> {
            Self::take(&self.work, visits)
        }

        pub fn alloc<T>(&self, count: usize) -> Result<(), Exhausted> {
            let bytes = count.checked_mul(size_of::<T>()).ok_or(Exhausted)?;
            Self::take(&self.bytes, bytes)
        }

        /// Optional acceleration does not consume the allowance for lookups.
        pub fn alloc_optional<T>(&self, count: usize) -> Result<(), Exhausted> {
            let bytes = count.checked_mul(size_of::<T>()).ok_or(Exhausted)?;
            Self::take(&self.optional_bytes, bytes)
        }

        fn take(counter: &AtomicUsize, amount: usize) -> Result<(), Exhausted> {
            if amount == 0 {
                return Ok(());
            }
            // `try_update` is newer than the MSRV; `fetch_update` is deprecated.
            let mut remaining = counter.load(Ordering::Relaxed);
            loop {
                let next = remaining.checked_sub(amount).ok_or(Exhausted)?;
                match counter.compare_exchange_weak(
                    remaining,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok(()),
                    Err(actual) => remaining = actual,
                }
            }
        }

        #[cfg(test)]
        pub fn remaining(&self) -> (usize, usize) {
            (
                self.work.load(Ordering::Relaxed),
                self.bytes.load(Ordering::Relaxed),
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn allowances_are_independent_and_cannot_wrap() {
            let budget = Budget::new(3, 8);
            assert_eq!(budget.work(3), Ok(()));
            assert_eq!(budget.work(1), Err(Exhausted));
            assert_eq!(budget.alloc::<u32>(2), Ok(()));
            assert_eq!(budget.alloc::<u32>(1), Err(Exhausted));
            assert_eq!(budget.alloc::<u32>(usize::MAX), Err(Exhausted));
            assert_eq!(budget.remaining(), (0, 0));
        }

        #[test]
        fn optional_allocations_do_not_consume_lookup_allowance() {
            let budget = Budget::new(3, 8);
            assert_eq!(budget.alloc_optional::<u32>(2), Ok(()));
            assert_eq!(budget.alloc_optional::<u8>(1), Err(Exhausted));
            assert_eq!(budget.alloc_optional::<u32>(usize::MAX), Err(Exhausted));
            assert_eq!(budget.remaining(), (3, 8));
            assert_eq!(budget.alloc::<u32>(2), Ok(()));
        }

        #[test]
        fn size_scaling_is_bounded() {
            assert_eq!(Budget::for_table_bytes(0).remaining(), (4 << 20, 4 << 20));
            assert_eq!(
                Budget::for_table_bytes(usize::MAX).remaining(),
                (128 << 20, 128 << 20)
            );
        }

        #[test]
        fn concurrent_builders_share_the_allowance() {
            let budget = Budget::new(1000, 1000);
            let spent = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            let mut spent = 0;
                            while budget.work(1).is_ok() {
                                spent += 1;
                            }
                            spent
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap())
                    .sum::<usize>()
            });
            assert_eq!(spent, 1000);
            assert_eq!(budget.remaining(), (0, 1000));
        }
    }
}

pub(crate) mod cache {
    use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU8, Ordering};

    /// Trait for atomics used in cache storage
    pub trait AtomicStorage: Sized {
        const BITS: usize;
        fn get(&self) -> u32;
        fn set(&self, val: u32);
        fn default() -> Self;
    }

    impl AtomicStorage for AtomicU8 {
        const BITS: usize = 8;

        fn get(&self) -> u32 {
            self.load(Ordering::Relaxed) as u32
        }

        fn set(&self, val: u32) {
            self.store(val as u8, Ordering::Relaxed);
        }

        fn default() -> Self {
            Self::new(u8::MAX)
        }
    }

    impl AtomicStorage for AtomicU16 {
        const BITS: usize = 16;

        fn get(&self) -> u32 {
            self.load(Ordering::Relaxed) as u32
        }

        fn set(&self, val: u32) {
            self.store(val as u16, Ordering::Relaxed);
        }

        fn default() -> Self {
            Self::new(u16::MAX)
        }
    }

    impl AtomicStorage for AtomicU32 {
        const BITS: usize = 32;

        fn get(&self) -> u32 {
            self.load(Ordering::Relaxed)
        }

        fn set(&self, val: u32) {
            self.store(val, Ordering::Relaxed);
        }

        fn default() -> Self {
            Self::new(u32::MAX)
        }
    }

    /// Selects correct type from STORAGE_BITS
    pub trait SelectAtomic<const BITS: usize> {
        type Type: AtomicStorage;
    }
    impl SelectAtomic<8> for () {
        type Type = AtomicU8;
    }
    impl SelectAtomic<16> for () {
        type Type = AtomicU16;
    }
    impl SelectAtomic<32> for () {
        type Type = AtomicU32;
    }

    /// Public wrapper
    pub type Cache<
        const KEY_BITS: usize,
        const VALUE_BITS: usize,
        const CACHE_SIZE: usize,
        const STORAGE_BITS: usize,
    > = CacheCore<KEY_BITS, VALUE_BITS, CACHE_SIZE, <() as SelectAtomic<STORAGE_BITS>>::Type>;

    /// Core cache
    #[derive(Debug)]
    pub struct CacheCore<
        const KEY_BITS: usize,
        const VALUE_BITS: usize,
        const CACHE_SIZE: usize,
        T: AtomicStorage,
    > {
        values: [T; CACHE_SIZE],
    }

    impl<
            const KEY_BITS: usize,
            const VALUE_BITS: usize,
            const CACHE_SIZE: usize,
            T: AtomicStorage,
        > CacheCore<KEY_BITS, VALUE_BITS, CACHE_SIZE, T>
    {
        pub const MAX_VALUE: u32 = (1 << VALUE_BITS) - 1;
        const CACHE_BITS: usize = CACHE_SIZE.ilog2() as usize;

        pub fn new() -> Self {
            debug_assert!(
                CACHE_SIZE.is_power_of_two(),
                "CACHE_SIZE must be a power of two"
            );

            debug_assert!(
                KEY_BITS >= Self::CACHE_BITS,
                "KEY_BITS must be >= log2(CACHE_SIZE)"
            );
            debug_assert!(
                KEY_BITS + VALUE_BITS <= Self::CACHE_BITS + T::BITS,
                "KEY_BITS + VALUE_BITS must fit in CACHE_BITS + T::BITS"
            );

            Self {
                values: core::array::from_fn(|_| T::default()),
            }
        }

        #[inline]
        pub fn get(&self, key: u32) -> Option<u32> {
            let index = (key as usize) & (CACHE_SIZE - 1);
            let stored = self.values[index].get();
            let tag = stored >> VALUE_BITS;
            let expected_tag = key >> Self::CACHE_BITS;

            if stored == T::default().get() || tag != expected_tag {
                return None;
            }

            Some(stored & ((1 << VALUE_BITS) - 1))
        }

        #[inline]
        pub fn set(&self, key: u32, value: u32) {
            if (key >> KEY_BITS) != 0 || (value >> VALUE_BITS) != 0 {
                return;
            }
            self.set_unchecked(key, value);
        }

        #[inline]
        fn set_unchecked(&self, key: u32, value: u32) {
            let index = (key as usize) & (CACHE_SIZE - 1);
            let packed = ((key >> Self::CACHE_BITS) << VALUE_BITS) | value;
            self.values[index].set(packed);
        }
    }

    impl<const KEY_BITS: usize, const VALUE_BITS: usize, const CACHE_SIZE: usize>
        CacheCore<KEY_BITS, VALUE_BITS, CACHE_SIZE, AtomicU16>
    {
        pub const fn empty() -> Self {
            Self {
                values: [const { AtomicU16::new(u16::MAX) }; CACHE_SIZE],
            }
        }
    }
}

pub(crate) mod hint {
    //! Branch hints for lookups that only fail on malformed input.

    /// Marks the `None` outcome of a lookup as unlikely.
    ///
    /// Branching on an `Option` otherwise looks to the optimizer like an even
    /// split, so a few early returns in a row make the code after them look
    /// rarely executed and keep it from being inlined or laid out as hot.
    pub(crate) trait Likely {
        fn likely(self) -> Self;
    }

    impl<T> Likely for Option<T> {
        #[inline(always)]
        fn likely(self) -> Self {
            if self.is_none() {
                cold();
            }
            self
        }
    }

    /// Converts a `Result` to an `Option`, marking the error as unlikely.
    pub(crate) trait LikelyOk<T> {
        fn likely_ok(self) -> Option<T>;
    }

    impl<T, E> LikelyOk<T> for Result<T, E> {
        #[inline(always)]
        fn likely_ok(self) -> Option<T> {
            if let Ok(value) = self {
                Some(value)
            } else {
                cold();
                None
            }
        }
    }

    // The body keeps the call from being optimized away along with its hint.
    #[cold]
    #[inline(never)]
    fn cold() {
        core::hint::black_box(());
    }
}

pub(crate) mod once {
    pub(crate) type Once<T> = std::sync::OnceLock<T>;
}

pub(crate) mod set_digest {
    use read_fonts::tables::layout::CoverageTable;

    type DigestMask = u64;

    const HB_SET_DIGEST_SHIFTS: [u32; 3] = [4, 0, 6];
    const N: usize = HB_SET_DIGEST_SHIFTS.len();
    const MASK_BITS: u32 = DigestMask::BITS;
    const MB1: u32 = MASK_BITS - 1;
    const ONE: DigestMask = 1;
    const ALL: DigestMask = DigestMask::MAX;

    #[derive(Clone, Debug)]
    pub struct SetDigest {
        masks: [DigestMask; N],
    }

    impl Default for SetDigest {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SetDigest {
        pub fn new() -> Self {
            Self { masks: [0; N] }
        }

        pub fn from_coverage(coverage: &CoverageTable) -> Self {
            let mut digest = Self::new();
            digest.add_coverage(coverage);
            digest
        }

        #[allow(dead_code)]
        pub fn clear(&mut self) {
            self.masks = [0; N];
        }

        pub fn full() -> Self {
            Self { masks: [ALL; N] }
        }

        pub fn union(&mut self, other: &Self) {
            for i in 0..N {
                self.masks[i] |= other.masks[i];
            }
        }

        pub fn add(&mut self, g: u32) {
            for i in 0..N {
                let shift = HB_SET_DIGEST_SHIFTS[i];
                let bit = (g >> shift) & MB1;
                self.masks[i] |= ONE << bit;
            }
        }

        pub fn add_array(&mut self, array: impl IntoIterator<Item = u32>) {
            for g in array {
                self.add(g);
            }
        }

        pub fn add_range(&mut self, a: u32, b: u32) -> bool {
            let a = a as DigestMask;
            let b = b as DigestMask;

            if self.masks.iter().all(|&m| m == ALL) {
                return false;
            }

            let mut changed = false;
            for i in 0..N {
                let shift = HB_SET_DIGEST_SHIFTS[i] as DigestMask;
                if (b >> shift).wrapping_sub(a >> shift) >= MB1 as DigestMask {
                    self.masks[i] = ALL;
                } else {
                    let ma = ONE << ((a >> shift) & MB1 as DigestMask);
                    let mb = ONE << ((b >> shift) & MB1 as DigestMask);
                    self.masks[i] |= mb + mb.wrapping_sub(ma) - DigestMask::from(mb < ma);
                    changed = true;
                }
            }
            changed
        }

        pub fn add_coverage(&mut self, coverage: &CoverageTable) {
            match coverage {
                CoverageTable::Format1(table) => {
                    for glyph in table.glyph_array() {
                        self.add(glyph.get().into());
                    }
                }
                CoverageTable::Format2(table) => {
                    for range in table.range_records() {
                        self.add_range(range.start_glyph_id().into(), range.end_glyph_id().into());
                    }
                }
                CoverageTable::Format3(table) => {
                    for glyph in table.glyph_array() {
                        self.add(glyph.get().to_u32());
                    }
                }
                CoverageTable::Format4(table) => {
                    for range in table.range_records() {
                        self.add_range(
                            range.start_glyph_id().to_u32(),
                            range.end_glyph_id().to_u32(),
                        );
                    }
                }
            }
        }

        pub fn may_have(&self, g: u32) -> bool {
            let bits = (self.masks[0] >> ((g >> HB_SET_DIGEST_SHIFTS[0]) & MB1))
                & (self.masks[1] >> ((g >> HB_SET_DIGEST_SHIFTS[1]) & MB1));
            if bits & ONE == 0 {
                return false;
            }
            (self.masks[2] >> ((g >> HB_SET_DIGEST_SHIFTS[2]) & MB1)) & ONE != 0
        }

        pub fn may_intersect(&self, other: &Self) -> bool {
            for i in 0..N {
                if self.masks[i] & other.masks[i] == 0 {
                    return false;
                }
            }
            true
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_single() {
            let mut set = SetDigest::new();
            set.add(2);
            assert!(set.may_have(2));
        }

        #[test]
        fn test_multiple_1() {
            let mut set = SetDigest::new();
            set.add(2);
            set.add(10);
            set.add(300);
            set.add(255);
            assert!(set.may_have(2));
            assert!(set.may_have(10));
            assert!(set.may_have(255));
            assert!(set.may_have(300));
        }

        #[test]
        fn test_multiple_2() {
            let mut set = SetDigest::new();
            set.add(245);
            set.add(1060);
            set.add(300);
            set.add(599);
            assert!(set.may_have(245));
            assert!(set.may_have(1060));
            assert!(set.may_have(300));
            assert!(set.may_have(599));
        }

        #[test]
        fn test_range_1() {
            let mut set = SetDigest::new();
            set.add_range(10, 12);
            assert!(set.may_have(10));
            assert!(set.may_have(11));
            assert!(set.may_have(12));
        }

        #[test]
        fn test_range_2() {
            let mut set = SetDigest::new();
            set.add_range(20, 15);
            set.add_range(15, 20);
            for gid in 15..=20 {
                assert!(set.may_have(gid));
            }
        }

        #[test]
        fn test_range_3() {
            let mut set = SetDigest::new();
            for i in 170..=239 {
                set.add(i);
            }
            assert!(set.may_have(200));
        }

        #[test]
        fn test_complex() {
            let mut set = SetDigest::new();
            set.add_range(5670, 5675);
            set.add(3);
            set.add(8769);
            set.add(10000);
            set.add_range(3456, 3460);

            assert!(set.may_have(3));
            assert!(set.may_have(5670));
            assert!(set.may_have(5675));
            assert!(set.may_have(8769));
            assert!(set.may_have(10000));
            assert!(set.may_have(3456));
            assert!(set.may_have(3460));
        }

        #[test]
        fn test_intersect() {
            let mut a = SetDigest::new();
            let mut b = SetDigest::new();

            a.add(123);
            b.add(456);
            assert!(!a.may_intersect(&b));

            b.add(123);
            assert!(a.may_intersect(&b));
        }
    }
}
