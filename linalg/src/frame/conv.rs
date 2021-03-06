use num_traits::Zero;
use std::fmt::Debug;
use std::ops::{Add, Mul};

use std::marker::PhantomData;

pub trait Conv<T: Copy + Add + Mul + Zero + Debug>: Send + Sync + Debug + objekt::Clone {
    fn packed_a_len(&self) -> usize;
    fn packed_a_alignment(&self) -> usize;
    fn pack_a(&self, pa: *mut T, a: *const T, rsa: isize, csa: isize);

    fn co(&self) -> usize;
    fn n(&self) -> usize;
    fn conv(&self, pa: *const T, b: *const T, c: *mut T, rsc: isize, csc: isize);
}

clone_trait_object!(<T> Conv<T> where T: Copy + Add + Mul + Zero);

pub trait ConvKer<T: Copy + Add + Mul + Zero>: Copy + Clone + Debug + Send + Sync {
    #[inline(always)]
    fn name() -> &'static str;
    #[inline(always)]
    fn kernel(
        k: usize,
        a: *const T,
        b_tops: *const *const T,
        b_down_offsets: *const isize,
        c: *mut T,
        rsc: usize,
        csc: usize,
    );
    #[inline(always)]
    fn mr() -> usize;
    #[inline(always)]
    fn nr() -> usize;
    #[inline(always)]
    fn alignment_bytes_a() -> usize;
    #[inline(always)]
    fn alignment_bytes_b() -> usize;
}

/// filters: O IHW packed as for matmul
/// "m" = O
/// "k" = I * Kh * Kw
///
/// data: unpacked

#[derive(Clone)]
pub struct PackedConv<K, T>
where
    K: ConvKer<T> + Debug,
    T: Copy + Add + Mul + Zero + Debug + Send + Sync,
{
    pub co: usize,
    pub kernel_offsets: Vec<isize>,
    pub k: usize,
    pub n: usize,
    pub data_offsets: Vec<isize>,
    _kernel: PhantomData<(K, T)>,
}

impl<K, T> std::fmt::Debug for PackedConv<K, T>
where
    K: ConvKer<T>,
    T: Copy + Add + Mul + Zero + Debug + Send + Sync,
{
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            fmt,
            "Conv co:{} k:{} centers:{} ({} {}x{})",
            self.co,
            self.kernel_offsets.len(),
            self.data_offsets.len(),
            K::name(),
            K::mr(),
            K::nr()
        )
    }
}

impl<K, T> PackedConv<K, T>
where
    K: ConvKer<T>,
    T: Copy + Add + Mul + Zero + Debug + Send + Sync,
{
    pub fn new(
        co: usize,
        mut kernel_offsets: Vec<isize>,
        mut data_offsets: Vec<isize>,
    ) -> PackedConv<K, T> {
        assert!(data_offsets.len() > 0);
        assert!(kernel_offsets.len() > 0);
        let k = kernel_offsets.len();
        let n = data_offsets.len();
        while data_offsets.len() % K::nr() != 0 {
            data_offsets.push(data_offsets[data_offsets.len() - 1]);
        }
        kernel_offsets.iter_mut().for_each(|x| *x *= 4);
        for _ in 0..4 {
            kernel_offsets.push(kernel_offsets[kernel_offsets.len() - 1]);
        }
        PackedConv { co, k, kernel_offsets, n, data_offsets, _kernel: PhantomData }
    }

    fn pack_panel_a(&self, pa: *mut T, a: *const T, rsa: isize, csa: isize, rows: usize) {
        let mr = K::mr();
        for i in 0..self.k {
            for j in 0..rows {
                unsafe {
                    *pa.offset((i * mr + j) as isize) =
                        *a.offset(i as isize * csa + j as isize * rsa)
                }
            }
        }
    }
}

impl<K, T> Conv<T> for PackedConv<K, T>
where
    K: ConvKer<T>,
    T: Copy + Add + Mul + Zero + Debug + Send + Sync + PartialEq,
{
    fn packed_a_alignment(&self) -> usize {
        K::alignment_bytes_a()
    }

    fn packed_a_len(&self) -> usize {
        let mr = K::mr();
        (self.co + mr - 1) / mr * mr * self.k
    }

    fn pack_a(&self, pa: *mut T, a: *const T, rsa: isize, csa: isize) {
        let mr = K::mr();
        assert!(pa as usize % K::alignment_bytes_a() == 0);
        unsafe {
            for p in 0..(self.co / mr) {
                self.pack_panel_a(
                    pa.offset((p * mr * self.k) as isize),
                    a.offset((p * mr) as isize * rsa),
                    rsa,
                    csa,
                    mr,
                )
            }
            if self.co % mr != 0 {
                self.pack_panel_a(
                    pa.offset((self.co / mr * mr * self.k) as isize),
                    a.offset((self.co / mr * mr) as isize * rsa),
                    rsa,
                    csa,
                    self.co % mr,
                )
            }
            assert_eq!(*pa, *a);
        }
    }

    fn conv(&self, pa: *const T, b: *const T, c: *mut T, rsc: isize, csc: isize) {
        assert!(pa as usize % K::alignment_bytes_a() == 0);
        let mr = K::mr();
        let nr = K::nr();
        let co = self.co;
        let k = self.k;
        let n = self.n;
        let mut tmpc = vec![T::zero(); mr * nr];
        unsafe {
            let btops: Vec<*const T> = self.data_offsets.iter().map(|&o| b.offset(o)).collect();
            for ia in 0..co / mr {
                for ib in 0..n / nr {
                    K::kernel(
                        k,
                        pa.offset((ia * k * mr) as isize),
                        btops.as_ptr().offset((ib * nr) as isize),
                        self.kernel_offsets.as_ptr(),
                        c.offset((mr * ia) as isize * rsc + (nr * ib) as isize * csc),
                        rsc as usize,
                        csc as usize,
                    );
                }
                if n % nr != 0 {
                    K::kernel(
                        k,
                        pa.offset((ia * k * mr) as isize),
                        btops.as_ptr().offset((n / nr * nr) as isize),
                        self.kernel_offsets.as_ptr(),
                        tmpc.as_mut_ptr(),
                        nr,
                        1,
                    );
                    for y in 0..mr {
                        for x in 0..(n % nr) {
                            *c.offset(
                                (mr * ia + y) as isize * rsc + (x + n / nr * nr) as isize * csc,
                            ) = tmpc[y * nr + x];
                        }
                    }
                }
            }
            if co % mr != 0 {
                for ib in 0..n / nr {
                    K::kernel(
                        k,
                        pa.offset((co / mr * mr * k) as isize),
                        btops.as_ptr().offset((ib * nr) as isize),
                        self.kernel_offsets.as_ptr(),
                        tmpc.as_mut_ptr(),
                        nr,
                        1,
                    );
                    for y in 0..(co % mr) {
                        for x in 0..nr {
                            *c.offset(
                                (y + co / mr * mr) as isize * rsc + (x + ib * nr) as isize * csc,
                            ) = tmpc[y * nr + x];
                        }
                    }
                }
                if n % nr != 0 {
                    K::kernel(
                        k,
                        pa.offset((co / mr * mr * k) as isize),
                        btops.as_ptr().offset((n / nr * nr) as isize),
                        self.kernel_offsets.as_ptr(),
                        tmpc.as_mut_ptr(),
                        nr,
                        1,
                    );
                    for y in 0..(co % mr) {
                        for x in 0..(n % nr) {
                            *c.offset(
                                (y + co / mr * mr) as isize * rsc
                                    + (x + n / nr * nr) as isize * csc,
                            ) = tmpc[y * nr + x];
                        }
                    }
                }
            }
        }
    }

    fn co(&self) -> usize {
        self.co
    }

    fn n(&self) -> usize {
        self.n
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::align;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    pub struct ConvProblem {
        pub ci: usize,
        pub co: usize,
        pub kt: usize,
        pub stride: usize,
        pub dilation: usize,
        pub filters: Vec<f32>,
        pub data: Vec<f32>,
    }

    impl ConvProblem {
        pub fn k(&self) -> usize {
            self.ci * self.kt
        }
        pub fn kernel_field(&self) -> usize {
            self.dilation * (self.kt - 1) + 1
        }
        pub fn input_width(&self) -> usize {
            self.data.len() / self.ci
        }
        pub fn output_width(&self) -> usize {
            (self.input_width() - self.kernel_field()) / self.stride + 1
        }
        pub fn offsets(&self) -> (Vec<isize>, Vec<isize>) {
            let data_offsets: Vec<isize> =
                (0..self.output_width()).map(|i| (i * self.stride) as isize).collect();
            let kernel_offsets: Vec<isize> = (0..self.ci)
                .flat_map(move |ici| {
                    (0..self.kt)
                        .map(move |ikt| (ikt * self.dilation + ici * self.input_width()) as isize)
                })
                .collect();
            (kernel_offsets, data_offsets)
        }
        pub fn expected(&self) -> Vec<f32> {
            let mut expect = vec![0.0f32; self.co * self.output_width()];
            for x in 0..self.output_width() {
                for ico in 0..self.co {
                    for ikt in 0..self.kt {
                        for ici in 0..self.ci {
                            let f = self.filters[ici * self.kt + ikt + self.ci * self.kt * ico];
                            let d = self.data
                                [x * self.stride + ikt * self.dilation + ici * self.input_width()];
                            expect[x + ico * self.output_width()] += f * d;
                        }
                    }
                }
            }
            expect
        }

        pub fn run<C: Conv<f32>>(&self, conv: &C) -> Vec<f32> {
            unsafe {
                let mut packed_a: Vec<f32> =
                    align::uninitialized(conv.packed_a_len(), conv.packed_a_alignment());
                conv.pack_a(packed_a.as_mut_ptr(), self.filters.as_ptr(), self.k() as isize, 1);

                let mut found = vec![9999.0f32; self.co * self.output_width()];
                conv.conv(
                    packed_a.as_ptr(),
                    self.data.as_ptr(),
                    found.as_mut_ptr(),
                    self.output_width() as isize,
                    1,
                );
                found
            }
        }
    }

    pub fn strat_conv_1d() -> BoxedStrategy<ConvProblem> {
        (1usize..40, 1usize..40, 1usize..10, 1usize..5, 1usize..5)
            .prop_flat_map(|(ci, co, kt, stride, dilation)| {
                let min = (kt - 1) * dilation + 1;
                (Just(ci), Just(co), Just(kt), Just(stride), Just(dilation), min..min + 10)
            })
            .prop_flat_map(move |(ci, co, kt, stride, dilation, t)| {
                (
                    Just(ci),
                    Just(co),
                    Just(kt),
                    Just(stride),
                    Just(dilation),
                    proptest::collection::vec((-10..10).prop_map(|a| a as f32), ci * co * kt),
                    proptest::collection::vec((-10..10).prop_map(|a| a as f32), t * ci),
                )
            })
            .prop_map(move |(ci, co, kt, stride, dilation, filters, data)| ConvProblem {
                ci,
                co,
                kt,
                stride,
                dilation,
                filters,
                data,
            })
            .boxed()
    }
}
