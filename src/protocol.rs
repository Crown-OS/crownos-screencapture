crownos_ipc::protocol! {
    pub mod protocol: "crownos-screenshot" {
        /// Finish the running recording and save it.
        notify stop();
    }
}
