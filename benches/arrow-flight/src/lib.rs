pub mod client;
mod codec;
pub mod server;

pub mod arrow {
    tonic::include_proto!("arrow.flight.protocol");

    pub mod manual {
        tonic::include_proto!("arrow.flight.protocol.FlightService");
    }
}
