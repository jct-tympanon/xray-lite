# xray

> [AWS X-Ray](https://aws.amazon.com/xray/) daemon client for Rust applications on [AWS Lambda](https://aws.amazon.com/lambda/)

## Installing

Add the following to your `Cargo.toml` file:

```toml
[dependencies]
xray = { git = "https://github.com/codemonger-io/xray.git", tag = "v0.0.1" }
```

## Usage

Here is an example to record a subsegment within a Lambda function invocation instrumented with AWS X-Ray:

```rust
use xray::{Client, Context, SubsegmentContext};

fn main() {
   // reads AWS_XRAY_DAEMON_ADDRESS
   let client = Client::from_lambda_env().unwrap();
   // reads _X_AMZN_TRACE_ID
   let context = SubsegmentContext::from_lambda_env(client).unwrap()
       .with_name_prefix("readme-example.");

   do_something(&context);
}

fn do_something(context: &dyn Context) {
    // subsegment will have the name "readme-example.do_something"
    let subsegment = context.enter_subsegment("do_something".to_string());

    // do something time consuming ...

    // the subsegment will be ended and reported when it is dropped
}
```

## Acknowledgements

[Doug Tangren (softprops)](https://github.com/softprops) 2018