pub mod proto {
    tonic::include_proto!("andler");
}

pub mod convert;

pub use convert::ConvertError;
