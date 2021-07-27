use abomonation::{decode, encode};
use alloc::borrow::ToOwned;
use alloc::{vec, vec::Vec};
use log::{debug, warn};

use smoltcp::iface::EthernetInterface;
use smoltcp::socket::{SocketHandle, SocketSet, TcpSocket, TcpSocketBuffer};
use smoltcp::time::Instant;
use smoltcp::wire::IpAddress;

use kpi::io::FileInfo;
use vmxnet3::smoltcp::DevQueuePhy;

use crate::cluster_api::{ClusterClientAPI, ClusterError, NodeId};
use crate::rpc::*;
use crate::rpc_api::RPCClientAPI;

const RX_BUF_LEN: usize = 4096;
const TX_BUF_LEN: usize = 4096;

pub struct TCPClient<'a> {
    iface: EthernetInterface<'a, DevQueuePhy>,
    sockets: SocketSet<'a>,
    server_handle: Option<SocketHandle>,
    server_ip: IpAddress,
    server_port: u16,
    client_port: u16,
    client_id: NodeId,
    req_id: u64,
}

impl TCPClient<'_> {
    pub fn new<'a>(
        server_ip: IpAddress,
        server_port: u16,
        iface: EthernetInterface<'a, DevQueuePhy>,
    ) -> TCPClient<'a> {
        TCPClient {
            iface: iface,
            sockets: SocketSet::new(vec![]),
            server_handle: None,
            server_ip: server_ip,
            server_port: server_port,
            client_port: 10110,
            client_id: 0,
            req_id: 0,
        }
    }
}

impl ClusterClientAPI for TCPClient<'_> {
    /// Register with controller, analogous to LITE join_cluster()
    /// TODO: add timeout?? with error returned if timeout occurs?
    fn join_cluster(&mut self) -> Result<NodeId, ClusterError> {
        // create client socket
        let tcp_rx_buffer = TcpSocketBuffer::new(vec![0; RX_BUF_LEN]);
        let tcp_tx_buffer = TcpSocketBuffer::new(vec![0; TX_BUF_LEN]);
        let tcp_socket = TcpSocket::new(tcp_rx_buffer, tcp_tx_buffer);
        self.server_handle = Some(self.sockets.add(tcp_socket));

        {
            let mut socket = self.sockets.get::<TcpSocket>(self.server_handle.unwrap());
            socket
                .connect((self.server_ip, self.server_port), self.client_port)
                .unwrap();
            debug!(
                "Attempting to connect to server {}:{}",
                self.server_ip, self.server_port
            );
        }

        // Connect to server
        loop {
            match self.iface.poll(&mut self.sockets, Instant::from_millis(0)) {
                Ok(_) => {}
                Err(e) => {
                    warn!("poll error: {}", e);
                }
            }
            let socket = self.sockets.get::<TcpSocket>(self.server_handle.unwrap());
            // Waiting for send/recv forces the TCP handshake to fully complete
            if socket.is_active() && (socket.may_send() || socket.may_recv()) {
                debug!("Connected to server, ready to send/recv data");
                break;
            }
        }

        self.rpc_call(0, RPCType::Registration, Vec::new()).unwrap();
        Ok(self.client_id)
    }
}

/// RPC client operations
impl RPCClientAPI for TCPClient<'_> {
    /// calls a remote RPC function with ID
    fn rpc_call(
        &mut self,
        pid: usize,
        rpc_id: RPCType,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, RPCError> {
        // Create request header
        let req_hdr = RPCHeader {
            client_id: self.client_id,
            pid: pid,
            req_id: self.req_id,
            msg_type: rpc_id,
            msg_len: data.len() as u64,
        };

        // Serialize request header then request body
        let mut req_data = Vec::new();
        unsafe { encode(&req_hdr, &mut req_data) }.unwrap();
        if data.len() > 0 {
            //unsafe { encode(&data, &mut req_data) }.unwrap();
            req_data.extend(data);
        }

        // Send request
        self.msg_send(req_data).unwrap();

        // Receive response and parse header
        let mut res = self.msg_recv().unwrap();
        let (res_hdr, res_body) = unsafe { decode::<RPCHeader>(&mut res) }.unwrap();

        // Check request & client IDs, and also length of received data
        if ((res_hdr.client_id != self.client_id) && rpc_id != RPCType::Registration)
            || res_hdr.req_id != self.req_id
        {
            warn!(
                "Mismatched client id ({}, {}) or request id ({}, {})",
                res_hdr.client_id, self.client_id, res_hdr.req_id, self.req_id
            );
            return Err(RPCError::MalformedResponse);
        } else if res_hdr.msg_len != (res_body.len() as u64) {
            warn!("Did not receive all RPC data!");
            return Err(RPCError::MalformedResponse);
        }

        // Increment request id
        self.req_id += 1;

        // If registration, update id
        if rpc_id == RPCType::Registration {
            self.client_id = res_hdr.client_id;
            debug!("Set client ID to: {}", self.client_id);
            return Ok(Vec::new());
        }

        Ok(res_body.to_vec())
    }

    /// send data to a remote node
    fn msg_send(&mut self, data: Vec<u8>) -> Result<(), RPCError> {
        // TODO: check TX capacity, chunk if necessary??

        let mut data_sent = false;
        loop {
            match self.iface.poll(&mut self.sockets, Instant::from_millis(0)) {
                Ok(_) => {}
                Err(e) => {
                    warn!("poll error: {}", e);
                }
            }

            let mut socket = self.sockets.get::<TcpSocket>(self.server_handle.unwrap());
            if socket.can_send() && !data_sent {
                socket.send_slice(&data[..]).unwrap();
                debug!("Client sent: {:?}", data);
                data_sent = true;
            } else if data_sent {
                return Ok(());
            }
        }
    }

    /// receive data from a remote node
    fn msg_recv(&mut self) -> Result<Vec<u8>, RPCError> {
        loop {
            match self.iface.poll(&mut self.sockets, Instant::from_millis(0)) {
                Ok(_) => {}
                Err(e) => {
                    warn!("poll error: {}", e);
                }
            }

            let mut socket = self.sockets.get::<TcpSocket>(self.server_handle.unwrap());
            if socket.can_recv() {
                // TODO: check rx capacity
                let data = socket
                    .recv(|buffer| {
                        let recvd_len = buffer.len();
                        let data = buffer.to_owned();
                        (recvd_len, data)
                    })
                    .unwrap();
                if data.len() > 0 {
                    debug!("Client recv: {:?}", data);
                    return Ok(data);
                }
            }
        }
    }
}

impl TCPClient<'_> {
    pub fn fio_write(
        &mut self,
        pid: usize,
        fd: u64,
        data: Vec<u8>,
    ) -> Result<(u64, u64), RPCError> {
        self.fio_writeat(pid, fd, 0, data)
    }

    pub fn fio_writeat(
        &mut self,
        pid: usize,
        fd: u64,
        offset: i64,
        data: Vec<u8>,
    ) -> Result<(u64, u64), RPCError> {
        let req = RPCRWReq {
            fd: fd,
            len: data.len() as u64,
            offset: offset,
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        req_data.extend(data);

        let mut res = self.rpc_call(pid, RPCType::WriteAt, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("Write() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_read(
        &mut self,
        pid: usize,
        fd: u64,
        len: u64,
        buff_ptr: &mut [u8],
    ) -> Result<(u64, u64), RPCError> {
        self.fio_readat(pid, fd, len, 0, buff_ptr)
    }

    pub fn fio_readat(
        &mut self,
        pid: usize,
        fd: u64,
        len: u64,
        offset: i64,
        buff_ptr: &mut [u8],
    ) -> Result<(u64, u64), RPCError> {
        let req = RPCRWReq {
            fd: fd,
            len: len,
            offset: offset,
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();

        let mut res = self.rpc_call(pid, RPCType::ReadAt, req_data).unwrap();
        if let Some((res, data)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            // If result is good, check how much data was returned
            if let Ok((bytes_read, _)) = res.ret {
                if bytes_read != data.len() as u64 {
                    warn!(
                        "Unexpected amount of data: bytes_read={:?}, data.len={:?}",
                        bytes_read,
                        data.len()
                    );
                    return Err(RPCError::MalformedResponse);

                // write data into user supplied buffer
                // TODO: more efficient way to write data?
                } else if bytes_read > 0 {
                    debug!("Read: {:?}", data);
                    buff_ptr.copy_from_slice(&data);
                }
                debug!("Read() {:?} {:?}", res, buff_ptr);
            }

            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_create(
        &mut self,
        pid: usize,
        pathname: &[u8],
        flags: u64,
        modes: u64,
    ) -> Result<(u64, u64), RPCError> {
        self.fio_open_create(pid, pathname, flags, modes, RPCType::Create)
    }

    pub fn fio_open(
        &mut self,
        pid: usize,
        pathname: &[u8],
        flags: u64,
        modes: u64,
    ) -> Result<(u64, u64), RPCError> {
        self.fio_open_create(pid, pathname, flags, modes, RPCType::Open)
    }

    fn fio_open_create(
        &mut self,
        pid: usize,
        pathname: &[u8],
        flags: u64,
        modes: u64,
        rpc_type: RPCType,
    ) -> Result<(u64, u64), RPCError> {
        let req = RPCOpenReq {
            pathname: pathname.to_vec(),
            flags: flags,
            modes: modes,
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        let mut res = self.rpc_call(pid, rpc_type, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("Open() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_close(&mut self, pid: usize, fd: u64) -> Result<(u64, u64), RPCError> {
        let req = RPCCloseReq { fd: fd };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();

        let mut res = self.rpc_call(pid, RPCType::Close, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("Close() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_delete(&mut self, pid: usize, pathname: &[u8]) -> Result<(u64, u64), RPCError> {
        let req = RPCDeleteReq {
            pathname: pathname.to_vec(),
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        let mut res = self.rpc_call(pid, RPCType::Delete, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("Delete() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_rename(
        &mut self,
        pid: usize,
        oldname: &[u8],
        newname: &[u8],
    ) -> Result<(u64, u64), RPCError> {
        let req = RPCRenameReq {
            oldname: oldname.to_vec(),
            newname: newname.to_vec(),
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        let mut res = self.rpc_call(pid, RPCType::FileRename, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("Rename() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_mkdir(
        &mut self,
        pid: usize,
        pathname: &[u8],
        modes: u64,
    ) -> Result<(u64, u64), RPCError> {
        let req = RPCMkDirReq {
            pathname: pathname.to_vec(),
            modes: modes,
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        let mut res = self.rpc_call(pid, RPCType::MkDir, req_data).unwrap();
        if let Some((res, remaining)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            if remaining.len() > 0 {
                return Err(RPCError::ExtraData);
            }
            debug!("MkDir() {:?}", res);
            return res.ret;
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }

    pub fn fio_getinfo(&mut self, pid: usize, name: &[u8]) -> Result<(u64, u64), RPCError> {
        let req = RPCGetInfoReq {
            name: name.to_vec(),
        };
        let mut req_data = Vec::new();
        unsafe { encode(&req, &mut req_data) }.unwrap();
        let mut res = self.rpc_call(pid, RPCType::GetInfo, req_data).unwrap();
        if let Some((_res, mut data)) = unsafe { decode::<FIORPCRes>(&mut res) } {
            // TODO: check parsed res
            if let Some((file_info, remaining)) = unsafe { decode::<FileInfo>(&mut data) } {
                if remaining.len() != 0 {
                    return Err(RPCError::ExtraData);
                }
                debug!("FileInfo() {:?}", file_info);
                return Ok((file_info.ftype, file_info.fsize));
            } else {
                return Err(RPCError::MalformedResponse);
            }
        } else {
            return Err(RPCError::MalformedResponse);
        }
    }
}