pub trait Platform {
    type Window: Window;

    fn create_window(&mut self) -> Self::Window;
}

pub trait Window {}
