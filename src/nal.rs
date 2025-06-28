use edge_nal::io::Error;
/// Re-export the `edge-nal` crate
pub use edge_nal::*;

/// Re-export the `edge-nal-std` crate
#[cfg(feature = "std")]
pub mod std {
    pub use edge_nal_std::*;

    impl super::NetStack for Stack {
        type Error = std::io::Error;

        type UdpBind<'a>
            = &'a Stack
        where
            Self: 'a;
        type UdpConnect<'a>
            = &'a Stack
        where
            Self: 'a;

        type TcpBind<'a>
            = &'a Stack
        where
            Self: 'a;
        type TcpConnect<'a>
            = &'a Stack
        where
            Self: 'a;

        type Dns<'a>
            = &'a Stack
        where
            Self: 'a;

        fn udp_bind(&self) -> Option<Self::UdpBind<'_>> {
            Some(self)
        }

        fn udp_connect(&self) -> Option<Self::UdpConnect<'_>> {
            Some(self)
        }

        fn tcp_bind(&self) -> Option<Self::TcpBind<'_>> {
            Some(self)
        }

        fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>> {
            Some(self)
        }

        fn dns(&self) -> Option<Self::Dns<'_>> {
            Some(self)
        }
    }
}

/// The network stack used by `rs-matter-stack`
///
/// Depending on the platform and features enabled, the stack
/// might or might not support certain protocols.
pub trait NetStack {
    type Error: Error;

    type UdpBind<'a>: UdpBind<Error = Self::Error>
    where
        Self: 'a;
    type UdpConnect<'a>: UdpConnect<Error = Self::Error>
    where
        Self: 'a;

    type TcpBind<'a>: TcpBind<Error = Self::Error>
    where
        Self: 'a;
    type TcpConnect<'a>: TcpConnect<Error = Self::Error>
    where
        Self: 'a;

    type Dns<'a>: Dns<Error = Self::Error>
    where
        Self: 'a;

    /// Return a UDP Bind implementation, if supported by the network stack
    fn udp_bind(&self) -> Option<Self::UdpBind<'_>>;

    /// Return a UDP Connect implementation, if supported by the network stack
    fn udp_connect(&self) -> Option<Self::UdpConnect<'_>>;

    /// Return a TCP Bind implementation, if supported by the network stack
    fn tcp_bind(&self) -> Option<Self::TcpBind<'_>>;

    /// Return a TCP Connect implementation, if supported by the network stack
    fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>>;

    /// Return a DNS implementation, if supported by the network stack
    fn dns(&self) -> Option<Self::Dns<'_>>;
}

impl<T> NetStack for &T
where
    T: NetStack,
{
    type Error = T::Error;

    type UdpBind<'a>
        = T::UdpBind<'a>
    where
        Self: 'a;
    type UdpConnect<'a>
        = T::UdpConnect<'a>
    where
        Self: 'a;

    type TcpBind<'a>
        = T::TcpBind<'a>
    where
        Self: 'a;
    type TcpConnect<'a>
        = T::TcpConnect<'a>
    where
        Self: 'a;

    type Dns<'a>
        = T::Dns<'a>
    where
        Self: 'a;

    fn udp_bind(&self) -> Option<Self::UdpBind<'_>> {
        (*self).udp_bind()
    }

    fn udp_connect(&self) -> Option<Self::UdpConnect<'_>> {
        (*self).udp_connect()
    }

    fn tcp_bind(&self) -> Option<Self::TcpBind<'_>> {
        (*self).tcp_bind()
    }

    fn tcp_connect(&self) -> Option<Self::TcpConnect<'_>> {
        (*self).tcp_connect()
    }

    fn dns(&self) -> Option<Self::Dns<'_>> {
        (*self).dns()
    }
}
