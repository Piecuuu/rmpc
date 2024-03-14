use std::{
    io::{BufRead, Read},
    str::FromStr,
};

use anyhow::Result;
use log::trace;

use super::{
    errors::{MpdError, MpdFailureResponse},
    split_line, FromMpd,
};
type MpdResult<T> = Result<T, MpdError>;

pub struct ProtoClient<'cmd, 'client, C: SocketClient> {
    command: &'cmd str,
    client: &'client mut C,
}

#[derive(Debug, Default, PartialEq)]
struct BinaryMpdResponse {
    pub bytes_read: u64,
    pub size_total: u32,
    pub mime_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MpdLine {
    Ok,
    Value(String),
}

impl<C: SocketClient> std::fmt::Debug for ProtoClient<'_, '_, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.command)
    }
}

pub trait SocketClient {
    fn reconnect(&mut self) -> MpdResult<&impl SocketClient>;
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn read(&mut self) -> &mut impl BufRead;
}

impl<'cmd, 'client, C: SocketClient> ProtoClient<'cmd, 'client, C> {
    pub fn new(input: &'cmd str, client: &'client mut C) -> Result<Self, MpdError> {
        let mut res = Self { command: input, client };
        res.execute(input)?;
        Ok(res)
    }

    fn execute(&mut self, command: &str) -> Result<&mut Self, MpdError> {
        if let Err(e) = self.client.write([command, "\n"].concat().as_bytes()) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                self.client.reconnect()?;
                self.client.write([command, "\n"].concat().as_bytes())?;
                Ok(self)
            } else {
                Err(e.into())
            }
        } else {
            Ok(self)
        }
    }

    pub(super) fn read_ok(mut self) -> Result<(), MpdError> {
        trace!(command = self.command; "Reading command");
        let read = self.client.read();
        match Self::read_line(read) {
            Ok(MpdLine::Ok) => Ok(()),
            Ok(MpdLine::Value(val)) => Err(MpdError::Generic(format!("Expected 'OK' but got '{val}'"))),
            Err(MpdError::ClientClosed) => {
                self.client.reconnect()?;
                self.execute(self.command)?;
                self.read_ok()
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn read_response<V>(mut self) -> Result<V, MpdError>
    where
        V: FromMpd + Default,
    {
        trace!(command = self.command; "Reading command");
        let mut result = V::default();
        let read = self.client.read();
        loop {
            match Self::read_line(read) {
                Ok(MpdLine::Ok) => return Ok(result),
                Ok(MpdLine::Value(val)) => result.next(val)?,
                Err(MpdError::ClientClosed) => {
                    self.client.reconnect()?;
                    self.execute(self.command)?;
                    return self.read_response::<V>();
                }
                Err(e) => return Err(e),
            };
        }
    }

    pub(super) fn read_opt_response<V>(mut self) -> Result<Option<V>, MpdError>
    where
        V: FromMpd + Default,
    {
        trace!(command = self.command; "Reading command");
        let mut result = V::default();
        let mut found_any = false;
        let read = self.client.read();
        loop {
            match Self::read_line(read) {
                Ok(MpdLine::Ok) => return if found_any { Ok(Some(result)) } else { Ok(None) },
                Ok(MpdLine::Value(val)) => {
                    found_any = true;
                    result.next(val)?;
                }
                Err(MpdError::ClientClosed) => {
                    self.client.reconnect()?;
                    self.execute(self.command)?;
                    return self.read_opt_response::<V>();
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub(super) fn read_bin(mut self) -> MpdResult<Option<Vec<u8>>> {
        let mut buf = Vec::new();
        let _ = match self._read_bin(&mut buf) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) => return Ok(None),
            Err(MpdError::ClientClosed) => {
                self.client.reconnect()?;
                self.execute(&format!("{} {}", self.command, buf.len()))?;
                self._read_bin(&mut buf)
            }
            Err(e) => Err(e),
        };
        loop {
            self.execute(&format!("{} {}", self.command, buf.len()))?;
            if let Some(response) = self._read_bin(&mut buf)? {
                if buf.len() >= response.size_total as usize || response.bytes_read == 0 {
                    trace!( len = buf.len();"Finshed reading binary response");
                    break;
                }
            } else {
                return Err(MpdError::ValueExpected("Expected binary data but got none".to_owned()));
            }
        }
        Ok(Some(buf))
    }

    fn _read_bin(&mut self, binary_buf: &mut Vec<u8>) -> Result<Option<BinaryMpdResponse>, MpdError> {
        let mut result = BinaryMpdResponse::default();
        let read = self.client.read();
        {
            loop {
                match Self::read_line(read)? {
                    MpdLine::Ok => {
                        log::warn!("Expected binary data but got 'OK'");
                        return Ok(None);
                    }
                    MpdLine::Value(val) => {
                        let (key, value) = split_line(val)?;
                        match key.to_lowercase().as_ref() {
                            "size" => result.size_total = value.parse()?,
                            "type" => result.mime_type = Some(value),
                            "binary" => {
                                result.bytes_read = value.parse()?;
                                break;
                            }
                            key => {
                                return Err(MpdError::Generic(format!(
                                    "Unexpected key when parsing binary response: '{key}'"
                                )))
                            }
                        }
                    }
                };
            }
        }
        let mut handle = read.take(result.bytes_read);
        let _ = handle.read_to_end(binary_buf)?;
        let _ = read.read_line(&mut String::new()); // MPD prints an empty new line at the end of binary response
        match Self::read_line(read)? {
            MpdLine::Ok => Ok(Some(result)),
            MpdLine::Value(val) => Err(MpdError::Generic(format!("Expected 'OK' but got '{val}'"))),
        }
    }

    fn read_line<R: BufRead>(read: &mut R) -> Result<MpdLine, MpdError> {
        let mut line = String::new();

        let bytes_read = match read.read_line(&mut line) {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(MpdError::ClientClosed),
            _ => Err(MpdError::ClientClosed),
        }?;

        if bytes_read == 0 {
            return Err(MpdError::ClientClosed);
        }

        if line.starts_with("OK") || line.starts_with("list_OK") {
            return Ok(MpdLine::Ok);
        }
        if line.starts_with("ACK") {
            return Err(MpdError::Mpd(MpdFailureResponse::from_str(&line)?));
        }
        line.pop(); // pop the new line
        Ok(MpdLine::Value(line))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::{BufReader, Cursor};

    use crate::mpd::{errors::MpdError, FromMpd, LineHandled};

    use super::SocketClient;

    #[derive(Default, Debug, PartialEq, Eq)]
    struct TestMpdObject {
        val_a: String,
        val_b: String,
    }
    impl FromMpd for TestMpdObject {
        fn next_internal(&mut self, key: &str, value: String) -> Result<LineHandled, MpdError> {
            if key == "fail" {
                return Err(MpdError::Generic(String::from("intentional fail")));
            }
            match key {
                "val_a" => self.val_a = value,
                "val_b" => self.val_b = value,
                _ => return Err(MpdError::Generic(String::from("unknown value"))),
            }
            Ok(LineHandled::Yes)
        }
    }

    struct TestClient {
        read: BufReader<Cursor<Vec<u8>>>,
    }
    impl TestClient {
        fn new(buf: &[u8]) -> Self {
            Self {
                read: BufReader::new(Cursor::new(buf.to_vec())),
            }
        }
    }
    impl SocketClient for TestClient {
        fn reconnect(&mut self) -> super::MpdResult<&impl SocketClient> {
            Ok(self)
        }
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn read(&mut self) -> &mut impl std::io::BufRead {
            &mut self.read
        }
    }

    mod read_mpd_line {

        use std::io::Cursor;

        use crate::mpd::{
            errors::{ErrorCode, MpdError, MpdFailureResponse},
            proto_client::{tests::TestClient, MpdLine, ProtoClient},
        };

        #[test]
        fn returns_ok() {
            let result = ProtoClient::<TestClient>::read_line(&mut Cursor::new(b"OK enenene"));

            assert_eq!(Ok(MpdLine::Ok), result);
        }

        #[test]
        fn returns_ok_for_list_ok() {
            let result = ProtoClient::<TestClient>::read_line(&mut Cursor::new(b"list_OK enenene"));

            assert_eq!(Ok(MpdLine::Ok), result);
        }

        #[test]
        fn returns_mpd_err() {
            let err = MpdFailureResponse {
                code: ErrorCode::PlayerSync,
                command_list_index: 2,
                command: "some_cmd".to_string(),
                message: "error message boi".to_string(),
            };

            let result =
                ProtoClient::<TestClient>::read_line(&mut Cursor::new(b"ACK [55@2] {some_cmd} error message boi"));

            assert_eq!(Err(MpdError::Mpd(err)), result);
        }

        #[test]
        fn returns_client_closed_on_broken_pipe() {
            struct Mock;
            impl std::io::BufRead for Mock {
                fn consume(&mut self, _amt: usize) {}
                fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                }
            }
            impl std::io::Read for Mock {
                fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                }
            }

            let result = ProtoClient::<TestClient>::read_line(&mut Mock);

            assert_eq!(Err(MpdError::ClientClosed), result);
        }
    }

    mod response {

        use crate::mpd::{
            errors::{ErrorCode, MpdError, MpdFailureResponse},
            proto_client::ProtoClient,
        };

        use super::*;

        #[test]
        fn parses_correct_response() {
            let buf: &[u8] = b"val_b: a\nval_a: 5\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_response::<TestMpdObject>();

            assert_eq!(
                result,
                Ok(TestMpdObject {
                    val_a: "5".to_owned(),
                    val_b: "a".to_owned()
                })
            );
        }

        #[test]
        fn returns_parse_error() {
            let buf: &[u8] = b"fail: lol\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_response::<TestMpdObject>();

            assert_eq!(result, Err(MpdError::Generic(String::from("intentional fail"))));
        }

        #[test]
        fn returns_mpd_error() {
            let buf: &[u8] = b"ACK [55@2] {some_cmd} error message boi\n";
            let err = MpdFailureResponse {
                code: ErrorCode::PlayerSync,
                command_list_index: 2,
                command: "some_cmd".to_string(),
                message: "error message boi".to_string(),
            };

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_response::<TestMpdObject>();

            assert_eq!(result, Err(MpdError::Mpd(err)));
        }
    }
    mod response_opt {
        use crate::mpd::{
            errors::{ErrorCode, MpdError, MpdFailureResponse},
            proto_client::ProtoClient,
        };

        use super::*;

        #[test]
        fn parses_correct_response() {
            let buf: &[u8] = b"val_b: a\nval_a: 5\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_opt_response::<TestMpdObject>();

            assert_eq!(
                result,
                Ok(Some(TestMpdObject {
                    val_a: "5".to_owned(),
                    val_b: "a".to_owned()
                }))
            );
        }

        #[test]
        fn returns_none() {
            let buf: &[u8] = b"OK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_opt_response::<TestMpdObject>();

            assert_eq!(result, Ok(None));
        }

        #[test]
        fn returns_parse_error() {
            let buf: &[u8] = b"fail: lol\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_opt_response::<TestMpdObject>();

            assert_eq!(result, Err(MpdError::Generic(String::from("intentional fail"))));
        }

        #[test]
        fn returns_mpd_error() {
            let buf: &[u8] = b"ACK [55@2] {some_cmd} error message boi\n";
            let err = MpdFailureResponse {
                code: ErrorCode::PlayerSync,
                command_list_index: 2,
                command: "some_cmd".to_string(),
                message: "error message boi".to_string(),
            };

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                .read_opt_response::<TestMpdObject>();

            assert_eq!(result, Err(MpdError::Mpd(err)));
        }
    }

    mod ok {
        use crate::mpd::{
            errors::{ErrorCode, MpdFailureResponse},
            proto_client::ProtoClient,
        };

        use super::*;

        #[test]
        fn parses_correct_response() {
            let buf: &[u8] = b"OK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf)).unwrap().read_ok();

            assert_eq!(result, Ok(()));
        }

        #[test]
        fn returns_mpd_error() {
            let buf: &[u8] = b"ACK [55@2] {some_cmd} error message boi\n";
            let err = MpdFailureResponse {
                code: ErrorCode::PlayerSync,
                command_list_index: 2,
                command: "some_cmd".to_string(),
                message: "error message boi".to_string(),
            };

            let result = ProtoClient::new("", &mut TestClient::new(buf)).unwrap().read_ok();

            assert_eq!(result, Err(MpdError::Mpd(err)));
        }

        #[test]
        fn returns_error_when_receiving_value() {
            let buf: &[u8] = b"idc\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf)).unwrap().read_ok();

            assert_eq!(
                result,
                Err(MpdError::Generic(String::from("Expected 'OK' but got 'idc'")))
            );
        }
    }

    mod binary {
        use crate::mpd::{
            errors::{ErrorCode, MpdError, MpdFailureResponse},
            proto_client::{tests::TestClient, BinaryMpdResponse, ProtoClient},
        };

        #[test]
        fn returns_mpd_error() {
            let buf: &[u8] = b"ACK [55@2] {some_cmd} error message boi\n";
            let err = MpdFailureResponse {
                code: ErrorCode::PlayerSync,
                command_list_index: 2,
                command: "some_cmd".to_string(),
                message: "error message boi".to_string(),
            };

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                ._read_bin(&mut Vec::new());

            assert_eq!(result, Err(MpdError::Mpd(err)));
        }

        #[test]
        fn returns_error_when_unknown_receiving_value() {
            let buf: &[u8] = b"idc: value\nOK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                ._read_bin(&mut Vec::new());

            assert_eq!(
                result,
                Err(MpdError::Generic(String::from(
                    "Unexpected key when parsing binary response: 'idc'"
                )))
            );
        }

        #[test]
        fn returns_none_when_unknown_receiving_unexpected_ok() {
            let buf: &[u8] = b"OK\n";

            let result = ProtoClient::new("", &mut TestClient::new(buf))
                .unwrap()
                ._read_bin(&mut Vec::new());

            assert_eq!(result, Ok(None));
        }

        #[test]
        fn returns_success() {
            let bytes = &[0; 111];
            let buf: &[u8] = b"size: 222\ntype: image/png\nbinary: 111\n";
            let buf_end: &[u8] = b"\nOK\n";
            let c = [buf, bytes, buf_end].concat();
            let mut client = TestClient::new(&c);
            let mut command = ProtoClient::new("", &mut client).unwrap();

            let mut buf = Vec::new();
            let result = command._read_bin(&mut buf);

            assert_eq!(buf, bytes);
            assert_eq!(
                result,
                Ok(Some(BinaryMpdResponse {
                    bytes_read: 111,
                    size_total: 222,
                    mime_type: Some("image/png".to_owned())
                }))
            );
        }
    }
}
