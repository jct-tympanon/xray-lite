use xray;

fn main() {
    let _client = xray::Client::from_lambda_env();
    println!("{}", xray::TraceId::new());
    println!("{}", xray::SegmentId::new());
}
