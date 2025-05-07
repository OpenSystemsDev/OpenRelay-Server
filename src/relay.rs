use crate::types::{
    ActiveConnections, BandwidthTracker, DeviceStatus, HardwareRateLimits, LastActive,
    MessageQueues, QueuedMessage, RateLimit, WebSocketMessage,
};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, time};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use warp::{
    Filter,
    ws::{Message, WebSocket},
};

// Store active connections with their Hardware IDs and message channels
// The server will use this to find the correct connection to send messages to
type Connections = DashMap<String, mpsc::UnboundedSender<Message>>;

pub struct RelayServer {
    addr: SocketAddr,
    connection_timeout: Duration,
    bandwidth_tracker: BandwidthTracker,
}

impl RelayServer {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            addr: addr.into(),
            connection_timeout: Duration::from_secs(30), // 30 second timeout
            bandwidth_tracker: BandwidthTracker::new("bandwidth_data.txt"),
        }
    }

    pub async fn run(&self) {
        // Create shared state
        let connections = Arc::new(Connections::new());
        let hardware_rate_limits = Arc::new(HardwareRateLimits::new());
        let message_queues = Arc::new(MessageQueues::new());
        let last_active = Arc::new(LastActive::new());
        let bandwidth_tracker = Arc::new(self.bandwidth_tracker.clone());
        let active_connections = Arc::new(ActiveConnections::new());

        // Create filters to pass state to handlers
        // Clone Arcs here so the originals are not moved into the closures
        let connections_clone = connections.clone();
        let hardware_rate_limits_clone = hardware_rate_limits.clone();
        let message_queues_clone = message_queues.clone();
        let last_active_clone = last_active.clone();
        let bandwidth_tracker_clone = bandwidth_tracker.clone();
        let active_connections_clone = active_connections.clone();

        let connections_filter = warp::any().map(move || connections_clone.clone());
        let hardware_rate_limits_filter =
            warp::any().map(move || hardware_rate_limits_clone.clone());
        let message_queues_filter = warp::any().map(move || message_queues_clone.clone());
        let last_active_filter = warp::any().map(move || last_active_clone.clone());
        let bandwidth_tracker_filter = warp::any().map(move || bandwidth_tracker_clone.clone());
        let active_connections_filter = warp::any().map(move || active_connections_clone.clone());

        // Define WebSocket route
        let ws_route = warp::path("relay")
            .and(warp::ws())
            .and(connections_filter.clone())
            .and(hardware_rate_limits_filter.clone())
            .and(message_queues_filter.clone())
            .and(last_active_filter.clone())
            .and(bandwidth_tracker_filter.clone())
            .and(active_connections_filter.clone())
            .map(
                |ws: warp::ws::Ws,
                 connections,
                 hardware_rate_limits,
                 message_queues,
                 last_active,
                 bandwidth_tracker,
                 active_connections| {
                    ws.on_upgrade(move |socket| {
                        handle_websocket(
                            socket,
                            connections,
                            hardware_rate_limits,
                            message_queues,
                            last_active,
                            bandwidth_tracker,
                            active_connections,
                        )
                    })
                },
            );

        // If its working
        let health = warp::path("health").map(|| "OK");

        // Bandwidth status endpoint
        let bandwidth_tracker_status = bandwidth_tracker.clone();
        let bandwidth_status = warp::path("bandwidth-status").map(move || {
            let (used, limit, percentage) = bandwidth_tracker_status.get_status();
            format!(
                "Bandwidth Usage: {:.2} TB / {:.2} TB ({:.2}%)",
                used as f64 / 1_000_000_000_000.0,
                limit as f64 / 1_000_000_000_000.0,
                percentage
            )
        });

        // Combine routes
        let routes = health.or(bandwidth_status).or(ws_route);

        // Start background tasks
        let cleanup_task = start_cleanup_task(
            connections.clone(),
            message_queues.clone(),
            last_active.clone(),
            active_connections.clone(),
            self.connection_timeout,
        );

        // Run the server
        info!("WebSocket relay server listening on: {}", self.addr);
        let server_task = warp::serve(routes).run(self.addr);

        // Wait for them
        tokio::select! {
            _ = server_task => {
                info!("Server stopped");
            }
            _ = cleanup_task => {
                error!("Cleanup task stopped unexpectedly");
            }
        }
    }
}

// Clean up inactive connections and expired messages
fn start_cleanup_task(
    connections: Arc<Connections>,
    message_queues: Arc<MessageQueues>,
    last_active: Arc<LastActive>,
    active_connections: Arc<ActiveConnections>,
    timeout: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(10));

        loop {
            interval.tick().await;

            // Check for timed out connections
            let now = Instant::now();
            let mut to_remove = Vec::new();

            for entry in last_active.iter() {
                let hardware_id = entry.key();
                let last_seen = entry.value();

                if now.duration_since(*last_seen) > timeout {
                    to_remove.push(hardware_id.clone());
                }
            }

            // Remove timed out connections
            for hardware_id in to_remove {
                debug!("Removing inactive connection for device: {}", hardware_id);
                connections.remove(&hardware_id);
                last_active.remove(&hardware_id);

                // Remove from active connections
                active_connections.remove(&hardware_id);
            }

            // Process message queues and remove expired messages
            for mut entry in message_queues.iter_mut() {
                let hardware_id = entry.key().clone();
                let queue = entry.value_mut();

                queue.retain(|msg| !msg.is_expired());

                // Try to deliver messages if device is now connected (Message Queueing feature)
                if let Some(tx) = connections.get(&hardware_id) {
                    let mut delivered = Vec::new();

                    for (i, queued) in queue.iter().enumerate() {
                        match tx.send(Message::text(
                            serde_json::to_string(&queued.message).unwrap_or_default(),
                        )) {
                            Ok(_) => {
                                debug!("Delivered queued message to device: {}", hardware_id);
                                delivered.push(i);
                            }
                            Err(e) => {
                                error!("Failed to deliver queued message: {}", e);
                                break;
                            }
                        }
                    }

                    // Remove delivered messages (in reverse to maintain indices)
                    for i in delivered.into_iter().rev() {
                        if i < queue.len() {
                            queue.remove(i);
                        }
                    }
                }

                // Remove entry if queue is empty
                if queue.is_empty() {
                    message_queues.remove(&hardware_id);
                }
            }
        }
    })
}

async fn handle_websocket(
    ws: WebSocket,
    connections: Arc<Connections>,
    hardware_rate_limits: Arc<HardwareRateLimits>,
    message_queues: Arc<MessageQueues>,
    last_active: Arc<LastActive>,
    bandwidth_tracker: Arc<BandwidthTracker>,
    active_connections: Arc<ActiveConnections>,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Generate a temporary connection ID until registration
    let connection_id = Uuid::new_v4().to_string();
    let mut hardware_id = String::new();

    info!(
        "New WebSocket connection established (conn id: {})",
        connection_id
    );

    // Set initial last active time
    last_active.insert(connection_id.clone(), Instant::now());

    // Check and reset bandwidth counter for the new month if needed
    bandwidth_tracker.check_and_reset_month();

    // Create channel for this connection
    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    // Task to forward messages to the WebSocket
    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            if let Err(e) = ws_tx.send(message).await {
                error!("Failed to send WebSocket message: {}", e);
                break;
            }
        }
    });

    // Store connection to allow sending messages later
    connections.insert(connection_id.clone(), tx.clone());

    // Process incoming WebSocket messages
    while let Some(result) = ws_rx.next().await {
        match result {
            Ok(msg) => {
                // Update last active time
                if let Some(mut entry) = last_active.get_mut(&connection_id) {
                    *entry = Instant::now();
                }

                // Track bandwidth usage
                let msg_size = msg.as_bytes().len();
                if !bandwidth_tracker.add_bytes(msg_size as u64) {
                    error!("Monthly bandwidth limit approaching (9.5TB/10TB). Message rejected.");
                    let _ = send_error(
                        &tx,
                        "Server has reached monthly bandwidth limit. Please try again next month.",
                        509,
                    );
                    continue;
                }

                let msg_text = match msg.to_str() {
                    Ok(text) => text,
                    Err(_) => continue, // Not a text message
                };

                // Parse WebSocket message
                let ws_message: WebSocketMessage = match serde_json::from_str(msg_text) {
                    Ok(msg) => msg,
                    Err(e) => {
                        error!("Failed to parse WebSocket message: {}", e);
                        let _ = send_error(&tx, "Invalid message format", 400);
                        continue;
                    }
                };

                // Process based on message type
                match ws_message {
                    WebSocketMessage::Register { hardware_id: hw_id } => {
                        // Store the hardware ID
                        hardware_id = hw_id.clone();

                        // Check if this hardware ID is already registered with another connection
                        if let Some(_existing_conn) = connections.get(&hardware_id) {
                            // If the device is already registered, update the connection
                            info!(
                                "Hardware ID already registered, updating connection: {}",
                                hardware_id
                            );

                            // Store connection with hardware ID as key
                            connections.insert(hardware_id.clone(), tx.clone());

                            // Get or create rate limits for this hardware ID
                            if !hardware_rate_limits.contains_key(&hardware_id) {
                                hardware_rate_limits
                                    .insert(hardware_id.clone(), RateLimit::default());
                            }

                            // Send registration response
                            let response = WebSocketMessage::RegisterResponse {
                                hardware_id: hardware_id.clone(),
                            };
                            let _ =
                                tx.send(Message::text(serde_json::to_string(&response).unwrap()));

                            // Check for queued messages
                            if let Some(queue) = message_queues.get(&hardware_id) {
                                if !queue.is_empty() {
                                    info!(
                                        "Delivering {} queued messages to {}",
                                        queue.len(),
                                        hardware_id
                                    );
                                }
                            }
                        } else {
                            // Store connection with hardware ID as key
                            connections.insert(hardware_id.clone(), tx.clone());

                            // Initialize rate limits for this hardware ID
                            hardware_rate_limits.insert(hardware_id.clone(), RateLimit::default());

                            info!("Device registered with Hardware ID: {}", hardware_id);

                            // Send registration response
                            let response = WebSocketMessage::RegisterResponse {
                                hardware_id: hardware_id.clone(),
                            };
                            let _ =
                                tx.send(Message::text(serde_json::to_string(&response).unwrap()));
                        }
                    }

                    WebSocketMessage::Send(relay_msg) => {
                        // Ensure we have a hardware ID
                        if hardware_id.is_empty() {
                            let _ = send_error(&tx, "Must register with hardware ID first", 403);
                            continue;
                        }

                        // Ensure the claimed hardware ID matches the registered one
                        if relay_msg.hardware_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in message: expected {}, got {}",
                                hardware_id, relay_msg.hardware_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        // Check if hardware ID has enough points for this message size
                        if let Some(mut rate_limit) = hardware_rate_limits.get_mut(&hardware_id) {
                            match rate_limit.check_and_consume(relay_msg.size_bytes) {
                                Ok(_) => {
                                    debug!(
                                        "Message {} accepted. Size: {}KB",
                                        relay_msg.message_id,
                                        (relay_msg.size_bytes / 1024) + 1
                                    );
                                }
                                Err(limit_message) => {
                                    let _ = send_error(&tx, &limit_message, 429);
                                    continue;
                                }
                            }
                        } else {
                            // Initialize rate limits if not exists
                            hardware_rate_limits.insert(hardware_id.clone(), RateLimit::default());
                            // Re-attempt consumption
                            if let Some(mut rate_limit) = hardware_rate_limits.get_mut(&hardware_id)
                            {
                                if let Err(limit_message) =
                                    rate_limit.check_and_consume(relay_msg.size_bytes)
                                {
                                    let _ = send_error(&tx, &limit_message, 429);
                                    continue;
                                }
                            }
                        }

                        // Track outgoing bandwidth for the relay operation
                        if !bandwidth_tracker.add_bytes(relay_msg.size_bytes) {
                            error!(
                                "Monthly bandwidth limit approaching (9.5TB/10TB). Message relay rejected."
                            );
                            let _ = send_error(
                                &tx,
                                "Server has reached monthly bandwidth limit. Please try again next month.",
                                509,
                            );
                            continue;
                        }

                        info!(
                            "Relaying message {} from {} to {}",
                            relay_msg.message_id, relay_msg.sender_id, relay_msg.recipient_id
                        );

                        // Check if recipient is connected by looking up its hardware ID
                        if let Some(recipient_tx) = connections.get(&relay_msg.recipient_id) {
                            // Relay the message to the recipient
                            let forward_msg = WebSocketMessage::Receive(relay_msg.clone());
                            match recipient_tx
                                .send(Message::text(serde_json::to_string(&forward_msg).unwrap()))
                            {
                                Ok(_) => {
                                    info!(
                                        "Message {} delivered successfully",
                                        relay_msg.message_id
                                    );
                                    // Send acknowledgment to sender
                                    let ack = WebSocketMessage::Ack {
                                        message_id: relay_msg.message_id.to_string(),
                                    };
                                    let _ = tx
                                        .send(Message::text(serde_json::to_string(&ack).unwrap()));
                                }
                                Err(e) => {
                                    error!("Failed to deliver message: {}", e);
                                    let _ = send_error(&recipient_tx, "Recipient unavailable", 503);
                                }
                            }
                        } else {
                            // Recipient not connected, queue message if TTL allows
                            if relay_msg.ttl > 0 {
                                debug!(
                                    "Queueing message for offline recipient: {}",
                                    relay_msg.recipient_id
                                );

                                let queued_msg = QueuedMessage::new(
                                    WebSocketMessage::Receive(relay_msg.clone()),
                                    relay_msg.ttl,
                                );

                                message_queues
                                    .entry(relay_msg.recipient_id.clone())
                                    .or_insert_with(Vec::new)
                                    .push(queued_msg);

                                // Send pending acknowledgment to sender
                                let ack = WebSocketMessage::Ack {
                                    message_id: relay_msg.message_id.to_string(),
                                };
                                let _ =
                                    tx.send(Message::text(serde_json::to_string(&ack).unwrap()));
                            } else {
                                // No queuing requested
                                let _ = send_error(&tx, "Recipient not connected", 404);
                            }
                        }
                    }

                    WebSocketMessage::AuthRequest {
                        recipient_id,
                        public_key,
                        challenge,
                        hardware_id: sender_hw_id,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in auth request: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!("Auth request from {} to {}", hardware_id, recipient_id);

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward auth request
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::AuthRequest {
                                    recipient_id: hardware_id.clone(),
                                    public_key,
                                    challenge,
                                    hardware_id: hardware_id.clone(),
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    debug!("Auth request forwarded to {}", recipient_id);
                                }
                                Err(e) => {
                                    error!("Failed to forward auth request: {}", e);
                                    let _ = send_error(&tx, "Recipient unavailable", 503);
                                }
                            }
                        } else {
                            // Recipient not connected
                            let _ = send_error(&tx, "Recipient not connected", 404);
                        }
                    }

                    WebSocketMessage::AuthResponse {
                        recipient_id,
                        challenge_response,
                        challenge,
                        hardware_id: sender_hw_id,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in auth response: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!("Auth response from {} to {}", hardware_id, recipient_id);

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward auth response
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::AuthResponse {
                                    recipient_id: hardware_id.clone(),
                                    challenge_response,
                                    challenge,
                                    hardware_id: hardware_id.clone(),
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    debug!("Auth response forwarded to {}", recipient_id);
                                }
                                Err(e) => {
                                    error!("Failed to forward auth response: {}", e);
                                    let _ = send_error(&tx, "Recipient unavailable", 503);
                                }
                            }
                        } else {
                            // Recipient not connected
                            let _ = send_error(&tx, "Recipient not connected", 404);
                        }
                    }

                    WebSocketMessage::AuthVerify {
                        recipient_id,
                        challenge_response,
                        hardware_id: sender_hw_id,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in auth verify: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!("Auth verification from {} to {}", hardware_id, recipient_id);

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward auth verification
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::AuthVerify {
                                    recipient_id: hardware_id.clone(),
                                    challenge_response,
                                    hardware_id: hardware_id.clone(),
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    debug!("Auth verification forwarded to {}", recipient_id);
                                }
                                Err(e) => {
                                    error!("Failed to forward auth verification: {}", e);
                                    let _ = send_error(&tx, "Recipient unavailable", 503);
                                }
                            }
                        } else {
                            // Recipient not connected
                            let _ = send_error(&tx, "Recipient not connected", 404);
                        }
                    }

                    WebSocketMessage::AuthSuccess {
                        recipient_id,
                        trusted,
                        hardware_id: sender_hw_id,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in auth success: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!(
                            "Auth success from {} to {}, trusted: {}",
                            hardware_id, recipient_id, trusted
                        );

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward auth success
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::AuthSuccess {
                                    recipient_id: hardware_id.clone(),
                                    trusted,
                                    hardware_id: hardware_id.clone(),
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    debug!("Auth success forwarded to {}", recipient_id);
                                }
                                Err(e) => {
                                    error!("Failed to forward auth success: {}", e);
                                    let _ = send_error(&tx, "Recipient unavailable", 503);
                                }
                            }
                        } else {
                            // Recipient not connected
                            let _ = send_error(&tx, "Recipient not connected", 404);
                        }
                    }

                    WebSocketMessage::KeyRotationUpdate {
                        hardware_id: sender_hw_id,
                        recipient_id,
                        encrypted_key_package,
                        key_id,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in key rotation: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!(
                            "Key rotation update from {} to {}",
                            hardware_id, recipient_id
                        );

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward the rotated key
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::KeyRotationUpdate {
                                    hardware_id: hardware_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    encrypted_key_package: encrypted_key_package.clone(),
                                    key_id,
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    info!("Key rotation update delivered successfully");
                                }
                                Err(e) => {
                                    error!("Failed to deliver key rotation update: {}", e);
                                    let _ = send_error(
                                        &tx,
                                        "Recipient unavailable for key rotation",
                                        503,
                                    );
                                }
                            }
                        } else {
                            // Queue key rotation update with high priority, if offline
                            debug!(
                                "Queueing key rotation for offline recipient: {}",
                                recipient_id
                            );

                            let queued_msg = QueuedMessage::new(
                                WebSocketMessage::KeyRotationUpdate {
                                    hardware_id: hardware_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    encrypted_key_package: encrypted_key_package.clone(),
                                    key_id,
                                },
                                300, // 5 minutes TTL for key updates
                            );

                            message_queues
                                .entry(recipient_id.clone())
                                .or_insert_with(Vec::new)
                                .push(queued_msg);

                            // Notify sender that message is queued
                            let _ =
                                send_error(&tx, "Key rotation queued for offline recipient", 202);
                        }
                    }

                    WebSocketMessage::KeyRotationAck {
                        hardware_id: sender_hw_id,
                        recipient_id,
                        key_id,
                        success,
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in key rotation ack: expected {}, got {}",
                                hardware_id, sender_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        info!(
                            "Key rotation acknowledgment from {} to {} for key {}",
                            hardware_id, recipient_id, key_id
                        );

                        // Find recipient's connection
                        if let Some(recipient_tx) = connections.get(&recipient_id) {
                            // Forward key rotation acknowledgment
                            match recipient_tx.send(Message::text(
                                serde_json::to_string(&WebSocketMessage::KeyRotationAck {
                                    hardware_id: hardware_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    key_id,
                                    success,
                                })
                                .unwrap(),
                            )) {
                                Ok(_) => {
                                    debug!("Key rotation ACK forwarded to {}", recipient_id);
                                }
                                Err(e) => {
                                    error!("Failed to forward key rotation ACK: {}", e);
                                    let _ = send_error(
                                        &tx,
                                        "Recipient unavailable for key rotation ACK",
                                        503,
                                    );
                                }
                            }
                        } else {
                            // Queue the ACK
                            debug!(
                                "Queueing key rotation ACK for offline recipient: {}",
                                recipient_id
                            );

                            let queued_msg = QueuedMessage::new(
                                WebSocketMessage::KeyRotationAck {
                                    hardware_id: hardware_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    key_id,
                                    success,
                                },
                                300, // 5 minutes TTL
                            );

                            message_queues
                                .entry(recipient_id.clone())
                                .or_insert_with(Vec::new)
                                .push(queued_msg);

                            // Notify sender that message is queued
                            let _ = send_error(
                                &tx,
                                "Key rotation acknowledgement queued for offline recipient",
                                202,
                            );
                        }
                    }

                    WebSocketMessage::Status {
                        hardware_id: status_hw_id,
                        status,
                    } => {
                        // Verify hardware ID matches the registered one
                        if status_hw_id != hardware_id {
                            warn!(
                                "Hardware ID mismatch in status update: expected {}, got {}",
                                hardware_id, status_hw_id
                            );
                            let _ = send_error(&tx, "Hardware ID mismatch", 403);
                            continue;
                        }

                        debug!("Device {} status changed to {:?}", hardware_id, status);

                        // If going offline, clean up resources
                        if status == DeviceStatus::Offline {
                            connections.remove(&connection_id);
                            last_active.remove(&connection_id);
                            active_connections.remove(&hardware_id);

                            // Remove hardware ID connection mapping
                            connections.remove(&hardware_id);
                        }
                    }

                    // Keep the websocket connection alive
                    WebSocketMessage::Ping => {
                        let pong = WebSocketMessage::Pong;
                        let _ = tx.send(Message::text(serde_json::to_string(&pong).unwrap()));
                    }

                    _ => {
                        // Handle other message types as needed
                        debug!("Received unhandled message type: {:?}", ws_message);
                    }
                }
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
        }
    }

    // Connection closed, remove from active connections
    connections.remove(&connection_id);
    last_active.remove(&connection_id);

    // Remove device connection but keep rate limits and message queues
    if !hardware_id.is_empty() {
        connections.remove(&hardware_id);
        active_connections.remove(&hardware_id);
    }

    info!(
        "WebSocket connection closed for hardware ID: {}",
        hardware_id
    );
}

// Helper function to send error responses
fn send_error(
    tx: &mpsc::UnboundedSender<Message>,
    message: &str,
    code: u16,
) -> Result<(), tokio::sync::mpsc::error::SendError<Message>> {
    let error_msg = WebSocketMessage::Error {
        message: message.to_string(),
        code: Some(code),
    };
    tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()))
}
