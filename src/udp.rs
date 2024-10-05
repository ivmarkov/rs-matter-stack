//! UDP transport implementation for edge-nal

use edge_nal::{Readable, UdpReceive, UdpSend};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::transport::network::{Address, NetworkReceive, NetworkSend};

/// UDP transport implementation for edge-nal
pub struct Udp<T>(pub T);

impl<T> NetworkSend for Udp<T>
where
    T: UdpSend,
{
    async fn send_to(&mut self, data: &[u8], addr: Address) -> Result<(), Error> {
        if let Address::Udp(remote) = addr {
            self.0.send(remote, data).await.map_err(map_err)?;

            Ok(())
        } else {
            Err(ErrorCode::NoNetworkInterface.into())
        }
    }
}

impl<T> NetworkReceive for Udp<T>
where
    T: UdpReceive + Readable,
{
    async fn wait_available(&mut self) -> Result<(), Error> {
        self.0.readable().await.map_err(map_err)?;

        Ok(())
    }

    async fn recv_from(&mut self, buffer: &mut [u8]) -> Result<(usize, Address), Error> {
        let (size, addr) = self.0.receive(buffer).await.map_err(map_err)?;

        Ok((size, Address::Udp(addr)))
    }
}

fn map_err<E>(_: E) -> Error {
    ErrorCode::StdIoError.into() // TODO
}
