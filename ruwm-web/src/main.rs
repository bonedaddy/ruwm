fn main() {
    wasm_logger::init(wasm_logger::Config::default());

    yew::start_app::<ruwm_web::App>();
}
