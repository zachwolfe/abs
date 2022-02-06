#[macro_export]
macro_rules! println_above_progress_bar_if_visible {
    ($progress_bar:expr, $($param:expr),+) => {
        let progress_bar: ProgressBar = $progress_bar.clone();
        if progress_bar.is_finished() {
            println!($($param),*);
        } else {
            progress_bar.println(format!($($param),*));
        }
    }
}