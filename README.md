# OpenRelay-Server
OpenRelay-Server is the backend relay server that allows OpenRelay to sync data across networks.

## Features
1.  **Zero-knowledge Architechture:** The server is designed to have zero knowledge of any of the data being 
    sent or received.
    *   Everything relayed is end-to-end encrypted by the client before being sent.
    *   Server operators have no ability to access, decrypt or store any message content during or after transmission.
2.  **WebSocket Communication:**
    *   Uses persistent secure websocket connections for low latency transmission of data.
    *   As a bonus, each device only requires one active connection to the server, as the server will broadcast messages to all the paired devices.
3.  **Device-to-Device Trust:** Authentication between devices are established directly between the devices    
    themselves via a cryptographic challenge (Refer to Security Measures below). The server simply facilitates the exchange of these challenge keys, but does not store or read them.
4.  **Key Rotation Support:** Allows for the automatic rotation of keys, even across networks. This is done by the client, and the server simply facilitates the exchange of these keys, but does not store or read them.
5.  **Usage based rate limiting :** The server is protected from abuse by both
    *   Restricting the number of requests per minute, per client to 60.
    *   Following a point-based system where each device will have a certain number of points that refresh every minute, and every kilobyte of data sent will cost a certain number of points.  
    If the device runs out of points, requests will be rejected until the points refresh.  
    This ensures that users can dynamically scale their usage based on their needs.  
    Note that rate limits are enforced via a separate identifier for each device, completely unrelated to the device id / ip address (Refer to Security Measures below). 
6. **Monthly Bandwidth Tracking:** Since the server is running on a Oracle Cloud instance, to keep it within 
    the free limits, I must adhere to the 10tb / month network limit. The server tracks the amount of data sent and received so far, and if it exceeds the acceptable set amount it begins rejecting every request, ensuring that I do not exceed the limit. You may edit the `monthly_limit` and `warning_threshold` in `types.rs` to change the limits, or remove them, if you are self-hosting the server.

## Security Measures
1.  **Device-to-Device Authentication and Trust:** Although the server facilitates the exchange, trust is directly established between the devices, and the server has no 
    knowledge. Here is how it works
-  **Key Generation:** When a new device connects to the OpenRelay server, it generates a unique **public / private key pair**. The private key **never** leaves the device.
-  **First Contact and Exchange of Public Keys:**
    *   When Device A (say) wants to connect to Device B (say) for the first time (or the other way around), they need each other's public keys. They exchange their public keys using the server.
-  **Challenge Initiation (A challenges B):**
    *   Device A generates a random, unique **challenge**.
    *   Device A sends an `AuthRequest` message to Device B (via the relay server) containing its own `device_id`, its `public_key`, and the `challenge`.
-  **Response Generation (B responds to A):**
    *   Device B receives the `AuthRequest`.
    *   Device B uses its own **private key** to sign the `challenge` it received from Device A. This signature proves Device B has the correct private key corresponding to its public key.
    *   Device B also generates its own unique `challenge` for Device A.
    *   Device B sends an `AuthResponse` message back to Device A (via the server) containing its `device_id`, the **signed response** to A's challenge, and its **new challenge** for A.
-  **Verification (A verifies B):**
    *   Device A receives the `AuthResponse`.
    *   Device A uses Device B's **public key** (which it obtained earlier) to verify the **signed response**.
    *   If the signature is valid for the original challenge A sent, Device A now cryptographically trusts that it's talking to the real Device B.
-  **Reciprocal Challenge (A responds to B's challenge):**
    *   Device A now uses its own **private key** to sign the **new challenge** it received from Device B in the `AuthResponse`.
    *   Device A sends an `AuthVerify` message to Device B (via the server) containing its `device_id` and the **signed response** to B's challenge.
-  **Final Verification (B verifies A):**
    *   Device B receives the `AuthVerify`.
    *   Device B uses Device A's **public key** (obtained in the initial `AuthRequest`) to verify the signature.
    *   If the signature is valid, Device B now cryptographically trusts Device A.
-  **Mutual Trust Established:** Both devices have now verified each other's identity by proving they hold the correct private keys.
2.  **Privacy-Preserving Rate Limiting:** Rate limits (both requests per minute and data volume points) are enforced based on the **temporary WebSocket connection session**, 
    identified by a unique ID generated *for that session only*. The server **does not** use the client's IP address or any persistent hardware / device identifier for rate limiting.
3.  **Ephemeral Data Handling (TTL & Cleanup):**
    *   **Message Time-To-Live (TTL):** Messages queued for offline recipients have a TTL set by the sender. If a message is not delivered before its TTL expires, it is automatically discarded by the server.
    *   **Background Cleanup:** A background service periodically scans and removes expired queued messages, cleans up resources associated with disconnected or timed-out client connections, so the server does not retain any data longer than what you set it for.

## Reponses
See [RESPONSES.md](//RESPONSES.md)

## Devlog
See [DEVLOG.md](//DEVLOG.md)

## Self-hosting
It is recommended to self-host the server, as it is a free service and I cannot guarantee uptime or availability.

```bash
cargo build --release
```

**Run:**

```bash
# Set optional port (default is 3000)
export PORT=8080

# Run the server
./target/release/openrelay-server
```

The server will start listening for WebSocket connections on the specified port (e.g., `ws://localhost:8080/relay` or `wss://yourdomain.com/relay` if behind a reverse proxy with TLS).

## Monitoring

*   **Health Check:** `GET /health` - Returns `OK` if the server is running.
*   **Bandwidth Status:** `GET /bandwidth-status` - Returns the current monthly bandwidth usage (e.g., `Bandwidth Usage: 0.01 TB / 10.00 TB (0.10%)`).

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the AGPL-3.0 License - see the LICENSE file for details.