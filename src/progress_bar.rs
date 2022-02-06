#[macro_export]
macro_rules! println_above_progress_bar_if_visible {
    ($progress_bar:expr, $($param:expr),+) => {
        let progress_bar: Option<ProgressBar> = $progress_bar.upgrade();
        if let Some(progress_bar) = progress_bar {
            progress_bar.println(format!($($param),*));
        } else {
            println!($($param),*);
        }
    }
}