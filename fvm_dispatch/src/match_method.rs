#[macro_export]
macro_rules! match_method {
    ($method:expr, {$($body:tt)*}) => {
        match_method!{@match $method, {}, $($body)*}
    };
    (@match $method:expr, {$($body:tt)*}, $(,)*) => {
        match $method {
            $($body)*
        }
    };
    (@match $method:expr, {$($body:tt)*}, $p:literal => $e:expr, $($tail:tt)*) => {
        match_method! {
            @match
            $method,
            {
                $($body)*
                $crate::method_hash!($p) => $e,
            },
            $($tail)*
        }
    };
    (@match $method:expr, {$($body:tt)*}, _ => $e:expr, $($tail:tt)*) => {
        match_method! {
            @match
            $method,
            {
                $($body)*
                _ => $e,
            },
            $($tail)*
        }
    };
}
