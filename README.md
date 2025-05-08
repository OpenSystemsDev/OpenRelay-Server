# OpenRelay-Server
OpenRelay-Server is the backend relay server that allows OpenRelay to sync data across networks.

# OpenRelay-Server
OpenRelay-Server is the backend relay server that allows OpenRelay to sync data across networks.

## Features
1.  **Zero-knowledge Architecture:** The server is designed to have zero knowledge of any of the data being 
    sent or received.
    *   Everything relayed is end-to-end encrypted by the client before being sent.
    *   Server admins have no ability to access, decrypt or store any message content during or after transmission.
2.  **Hardware ID based Authentication**: Devices identify themselves using a SHA256 / MD5 hash of their device hardware information (CPU, Motherboard, BIOS information and Device ID). This ensures that  
    *   Devices saved are persistent, and do not need to be re-paired, across restarts
    *   No email, phone number, or password based authentication is required to register / connect to a device.
    *   Messages and connections are routed directly using hardware IDs, making the system resilient to disconnections
    *   Offline messaging is indexed by hardware ID for reliable delivery when devices reconnect
3.  **WebSocket Communication:**  
    *   Uses persistent secure websocket connections for low latency transmission of data.  
4.  **Device-to-Device Trust:** Authentication between devices are established directly between the devices    
    themselves via a cryptographic challenge (Refer to Security Measures below). The server simply facilitates the exchange of these challenge keys, but does not store or read them.  
5.  **Key Rotation Support:** Allows for the automatic rotation of keys, even across networks. This is done by the client, and the server simply facilitates the exchange of these keys, but does not store or read them.  
6.  **Dual dynamic rate limiting system:** The server is protected from abuse by both  
    *   Restricting the number of requests per minute, per client to 60.
    *   Following a point-based system where each device will have a certain number of points that refresh every minute, and every kilobyte of data sent will cost a certain number of points.  
    If the device runs out of points, requests will be rejected until the points refresh.  
    This ensures that users can dynamically scale their usage based on their needs.  
    Note that rate limits are not enforced via IP address or any other identifiable information, but instead using the Hardware ID of the device sending the message (refer to Security Measures below).
7. **Monthly Bandwidth Tracking:** Since the server is running on an instance with limited monthly bandwidth, to 
    keep it within the free limits, I must adhere to the 10tb / month network limit. The server tracks the amount of data sent and received so far, and if it exceeds the acceptable set amount it begins rejecting every request, ensuring that I do not exceed the limit. You may edit the `monthly_limit` and `warning_threshold` in `types.rs` to change the limits, or remove them, if you are self-hosting the server.

## Security Measures
1.  **Device-to-Device Authentication and Trust:** Although the server facilitates the exchange, trust is directly established between the devices, and the server has no 
    knowledge. Here is how it works
-  **Key Generation:** When a new device connects to the OpenRelay server, it generates a unique **public / private key pair**. The private key **never** leaves the device.
-  **First Contact and Exchange of Public Keys:**
    *   When device A wants to pair with device B, they exchange their public keys securely via the server.
-  **Challenge Initiation (A challenges B):**
    *   Device A generates a random, unique **challenge**.
    *   Device A initiates the process by sending an AuthRequest message through the relay server containing its `device_id`, `public_key` and the randomly generated `challenge`.
-  **Response Generation (B responds to A):**
    *   Device B receives the `AuthRequest`.
    *   Device B uses its **private key** to sign the `challenge` it received from Device A. The signature proves that Device B holds the proper private key which matches its public key.
    *   Device B also generates its own unique `challenge` for Device A.
    *   Device B sends an `AuthResponse` message back to the server, which is sent to Device A containing its `device_id`, the `signed response` and a new `challenge`.
-  **Verification (A verifies B):**
    *   Device A receives the `AuthResponse`.
    *   Device A uses Device B's **public key** (which it obtained earlier) to verify the **signed response**.
    *   If the signature is valid for the original challenge A sent, Device A now cryptographically trusts that it's talking to the real Device B.
-  **Reciprocal Challenge (A responds to B's challenge):**
    *   Device A now uses its own **private key** to sign the **new challenge** it received from Device B in the `AuthResponse`.
    *   Device A sends an `AuthVerify` message to Device B (via the server) containing its `device_id` and the **signed response** to B's challenge.
-  **Final Verification (B verifies A):**
    *   Device B receives the `AuthVerify`.
    *   Device B verifies the signature using Device A's **public key** (obtained from the `AuthRequest` message earlier). If it's valid, Device B has now established a cryptographic trust with Device A.
-  **Mutual Trust Established:** Both devices have now verified each other's identity by proving they hold the correct private keys.
2.  **Privacy-Preserving Rate Limiting:** Rate limits (both requests per minute and data points) are enforced based on the **hardware ID hash** of the user's system provided during registration. This ensures that connections persist across client restarts, and connection cycling, reconnections cannot be used to abuse the server. The hardware ID is a one-way +hash obtained from the CPU, motherboard, BIOS, and system ID that cannot be reversed to identify the actual hardware information.
3.  **Ephemeral Data Handling (TTL & Cleanup):**
    *   **Message Time-To-Live (TTL):** Messages queued for offline recipients have a TTL set by the sender. If a message is not delivered before its TTL expires, it is automatically discarded by the server.
    *   **Background Cleanup:** A background service periodically scans and removes expired queued messages, cleans up resources associated with disconnected or timed-out client connections, so the server does not retain any data longer than what you set it for.

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
Note: To create a secure wss:// or https:// comnnection to a server outside your local network, you will need to setup a reverse proxy. OpenSystems personally employs and recommends Nginx for this purpose. See [NGINX.md](/NGINX.md) for more information.

## Monitoring

*   **Health Check:** `GET /health` - Returns `OK` if the server is running.
*   **Bandwidth Status:** `GET /bandwidth-status` - Returns the current monthly bandwidth usage (e.g., `Bandwidth Usage: 0.01 TB / 10.00 TB (0.10%)`).

## Reponses
See [RESPONSES.md](/RESPONSES.md)

## Devlog
See [DEVLOG.md](/DEVLOG.md)

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the AGPL-3.0 License - see the LICENSE file for details.
