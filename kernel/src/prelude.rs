#[macro_export]
macro_rules! round_up {
    ($num:expr, $s:expr) => {
        (($num + $s - 1) / $s) * $s
    };
}

pub trait PowersOf2 {
    fn log2(self) -> u8;
}

impl PowersOf2 for usize {
    #[cfg(target_pointer_width = "64")]
    fn log2(self) -> u8 {
        63 - self.leading_zeros() as u8
    }

    #[cfg(target_pointer_width = "32")]
    fn log2(self) -> u8 {
        31 - self.leading_zeros() as u8
    }
}
