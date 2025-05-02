use crate::types::{ 
    BandwidthTracker, DeviceStatus, QueuedMessage, RateLimit, 
    MessageQueues, LastActive, WebSocketMessage, HardwareIdMap, 
    HardwareRateLimits, DeviceIdToHwIdMap, ActiveConnections, AuthorizedDevices 
};
use base64;
use dashmap::DashMap;
use futures_util::{ SinkExt, StreamExt };
use std::{ net::SocketAddr, sync::Arc, time::{Duration, Instant} };
use tokio::{ sync::mpsc, time };
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{ debug, error, info, warn };
use uuid::Uuid;
use warp::{ ws::{Message, WebSocket}, Filter };

// Store active connections with their device IDs and message channels
// The server will use this to find the correct connection to send messages to
type Connections = DashMap<String, mpsc::UnboundedSender<Message>>;

pub struct RelayServer {
    addr: SocketAddr,
    connection_timeout: Duration,
    queue_ttl: Duration,
    bandwidth_tracker: BandwidthTracker,
}

impl RelayServer {
    pub fn new(addr: impl Into<SocketAddr>) -> Self {
        Self {
            addr: addr.into(),
            connection_timeout: Duration::from_secs(30), // 30 second timeout
            queue_ttl: Duration::from_secs(300),         // 5 minute TTL for queued messages
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
        let hardware_id_map = Arc::new(HardwareIdMap::new());
        let device_to_hwid_map = Arc::new(DeviceIdToHwIdMap::new());
        let active_connections = Arc::new(ActiveConnections::new());
        let authorized_devices = Arc::new(AuthorizedDevices::new());

        // Create filters to pass state to handlers
        // Clone Arcs here so the originals are not moved into the closures
        let connections_clone = connections.clone();
        let hardware_rate_limits_clone = hardware_rate_limits.clone();
        let message_queues_clone = message_queues.clone();
        let last_active_clone = last_active.clone();
        let bandwidth_tracker_clone = bandwidth_tracker.clone();
        let hardware_id_map_clone = hardware_id_map.clone();
        let device_to_hwid_map_clone = device_to_hwid_map.clone();
        let active_connections_clone = active_connections.clone();
        let authorized_devices_clone = authorized_devices.clone();

        let connections_filter = warp::any().map(move || connections_clone.clone());
        let hardware_rate_limits_filter = warp::any().map(move || hardware_rate_limits_clone.clone());
        let message_queues_filter = warp::any().map(move || message_queues_clone.clone());
        let last_active_filter = warp::any().map(move || last_active_clone.clone());
        let bandwidth_tracker_filter = warp::any().map(move || bandwidth_tracker_clone.clone());
        let hardware_id_map_filter = warp::any().map(move || hardware_id_map_clone.clone());
        let device_to_hwid_map_filter = warp::any().map(move || device_to_hwid_map_clone.clone());
        let active_connections_filter = warp::any().map(move || active_connections_clone.clone());
        let authorized_devices_filter = warp::any().map(move || authorized_devices_clone.clone());

        // Define WebSocket route
        let ws_route = warp::path("relay")
            .and(warp::ws())
            .and(connections_filter.clone())
            .and(hardware_rate_limits_filter.clone())
            .and(message_queues_filter.clone())
            .and(last_active_filter.clone())
            .and(bandwidth_tracker_filter.clone())
            .and(hardware_id_map_filter.clone())
            .and(device_to_hwid_map_filter.clone())
            .and(active_connections_filter.clone())
            .and(authorized_devices_filter.clone())
            .map(|ws: warp::ws::Ws, 
                 connections, 
                 hardware_rate_limits, 
                 message_queues, 
                 last_active,
                 bandwidth_tracker,
                 hardware_id_map,
                 device_to_hwid_map,
                 active_connections,
                 authorized_devices| {
                ws.on_upgrade(move |socket| {
                    handle_websocket(
                        socket, 
                        connections, 
                        hardware_rate_limits, 
                        message_queues, 
                        last_active,
                        bandwidth_tracker,
                        hardware_id_map,
                        device_to_hwid_map,
                        active_connections,
                        authorized_devices,
                    )
                })
            });

        // If its working
        let health = warp::path("health").map(|| "OK");

        // Bandwidth status endpoint
        let bandwidth_tracker_status = bandwidth_tracker.clone();
        let bandwidth_status = warp::path("bandwidth-status").map(move || {
            let (used, limit, percentage) = bandwidth_tracker_status.get_status();
            format!("Bandwidth Usage: {:.2} TB / {:.2} TB ({:.2}%)", 
                    used as f64 / 1_000_000_000_000.0,
                    limit as f64 / 1_000_000_000_000.0,
                    percentage)
        });

        // Combine routes
        let routes = health.or(bandwidth_status).or(ws_route);

        // Start background tasks
        let cleanup_task = start_cleanup_task(
            connections.clone(),
            message_queues.clone(),
            last_active.clone(),
            hardware_id_map.clone(),
            device_to_hwid_map.clone(),
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

fn check_auth_exempt_message(encrypted_data: &str) -> Result<bool, Box<dyn std::error::Error>> {
    // Try to decode the base64 data
    let bytes = match base64::decode(encrypted_data) {
        Ok(b) => b,
        Err(_) => return Ok(false), // Not base64, not exempt
    };
    
    // Try to parse as UTF-8 string
    let data_str = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(false), // Not UTF-8, not exempt
    };
    
    // Try to parse as JSON
    let json: serde_json::Value = match serde_json::from_str(data_str) {
        Ok(j) => j,
        Err(_) => return Ok(false), // Not JSON, not exempt
    };
    
    // Check for "type" field
    if let Some(msg_type) = json.get("type").and_then(|t| t.as_str()) {
        // List of message types that are exempt from authorization check
        let exempt_types = vec![
            "PairingRequest", 
            "PairingResponse",
            "AuthRequest", 
            "AuthResponse", 
            "AuthVerify", 
            "AuthSuccess",
            "AuthChallenge",
            "ClipboardUpdate",  // Add clipboard updates as an exempt type
            "ClipboardChunk"    // Add chunks as well
        ];
        
        return Ok(exempt_types.contains(&msg_type));
    }
    
    Ok(false)
}


// Clean up inactive connections and expired messages
fn start_cleanup_task(
    connections: Arc<Connections>,
    message_queues: Arc<MessageQueues>,
    last_active: Arc<LastActive>,
    hardware_id_map: Arc<HardwareIdMap>,
    device_to_hwid_map: Arc<DeviceIdToHwIdMap>,
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
                let device_id = entry.key();
                let last_seen = entry.value();
                
                if now.duration_since(*last_seen) > timeout {
                    to_remove.push(device_id.clone());
                }
            }
            
            // Remove timed out connections
            for device_id in to_remove {
                debug!("Removing inactive connection for device: {}", device_id);
                connections.remove(&device_id);
                last_active.remove(&device_id);
                
                // Remove from active connections
                active_connections.remove(&device_id);
                
                // Remove hardware ID mapping, but don't remove pairings
                if let Some(hw_id) = device_to_hwid_map.get(&device_id) {
                    hardware_id_map.remove(hw_id.value());
                }
                device_to_hwid_map.remove(&device_id);
            }
            
            // Process message queues and remove expired messages
            for mut entry in message_queues.iter_mut() {
                let device_id = entry.key().clone();
                let queue = entry.value_mut();
                
                queue.retain(|msg| !msg.is_expired());
                
                // Try to deliver messages if device is now connected (Message Queueing feature)
                if let Some(tx) = connections.get(&device_id) {
                    let mut delivered = Vec::new();
                    
                    for (i, queued) in queue.iter().enumerate() {
                        match tx.send(Message::text(
                            serde_json::to_string(&queued.message).unwrap_or_default(),
                        )) {
                            Ok(_) => {
                                debug!("Delivered queued message to device: {}", device_id);
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
                    message_queues.remove(&device_id);
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
    hardware_id_map: Arc<HardwareIdMap>,
    device_to_hwid_map: Arc<DeviceIdToHwIdMap>,
    active_connections: Arc<ActiveConnections>,
    authorized_devices: Arc<AuthorizedDevices>,
) {
    let (mut ws_tx, mut ws_rx) = ws.split();

    // Generate a temporary connection ID until registration
    let connection_id = Uuid::new_v4().to_string();
    let mut device_id = connection_id.clone();
    info!("New WebSocket connection established (conn id: {})", connection_id);

    // Hardware ID will be assigned during registration
    let mut hardware_id = String::new();
    
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
                    let error_msg = WebSocketMessage::Error { 
                        message: "Server has reached monthly bandwidth limit. Please try again next month.".to_string(),
                        code: Some(509), // 509 Bandwidth Limit Exceeded
                    };
                    let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
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
                        let error_msg = WebSocketMessage::Error {
                            message: "Invalid message format".to_string(),
                            code: Some(400),
                        };
                        // log e
                        error!("Error: {}", e);
                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                        continue;
                    }
                };

                // Process based on message type
                match ws_message {
                    WebSocketMessage::Register { hardware_id: hw_id } => {
                        // Store the hardware ID
                        hardware_id = hw_id.clone();
                        
                        // Check if this hardware ID is already registered with another connection
                        if let Some(existing_device_id) = hardware_id_map.get(&hardware_id) {
                            // If the device is already registered, update the connection
                            info!("Hardware ID already registered, updating connection for device ID: {}", existing_device_id.value());
                            
                            // Update device ID
                            device_id = existing_device_id.value().clone();
                            
                            // Update active connection mapping
                            active_connections.insert(device_id.clone(), connection_id.clone());
                            
                            // Get or create rate limits for this hardware ID
                            if !hardware_rate_limits.contains_key(&hardware_id) {
                                hardware_rate_limits.insert(hardware_id.clone(), RateLimit::default());
                            }
                            
                            // Send registration response
                            let response = WebSocketMessage::RegisterResponse {
                                device_id: device_id.clone(),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&response).unwrap()));
                            
                            // Check for queued messages
                            if let Some(queue) = message_queues.get(&device_id) {
                                if !queue.is_empty() {
                                    info!("Delivering {} queued messages to {}", queue.len(), device_id);
                                }
                            }
                        } else {
                            // Generate a new permanent device ID
                            device_id = Uuid::new_v4().to_string();
                            
                            // Store hardware ID to device ID mapping
                            hardware_id_map.insert(hardware_id.clone(), device_id.clone());
                            device_to_hwid_map.insert(device_id.clone(), hardware_id.clone());
                            
                            // Update active connection mapping
                            active_connections.insert(device_id.clone(), connection_id.clone());
                            
                            // Initialize rate limits for this hardware ID
                            hardware_rate_limits.insert(hardware_id.clone(), RateLimit::default());
                            
                            info!("Device registered with ID: {}, Hardware ID: {}", device_id, hardware_id);
                            
                            // Send registration response
                            let response = WebSocketMessage::RegisterResponse {
                                device_id: device_id.clone(),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&response).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::Send(mut relay_msg) => {
                        // Ensure we have a hardware ID
                        if hardware_id.is_empty() {
                            let error_msg = WebSocketMessage::Error {
                                message: "Must register with hardware ID first".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        // Ensure the claimed hardware ID matches the registered one
                        if relay_msg.hardware_id != hardware_id {
                            warn!("Hardware ID mismatch in message: expected {}, got {}", hardware_id, relay_msg.hardware_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        // Check message content to determine if it's a pairing or auth-related message
                        let is_auth_message = match check_auth_exempt_message(&relay_msg.encrypted_data) {
                            Ok(is_exempt) => is_exempt,
                            Err(_) => false, // If we can't determine, assume it's not exempt
                        };
                    
                        // Check if sender and recipient are paired/authorized (skip for auth-related messages)
                        if !is_auth_message {
                            let recipient_hw_id = match device_to_hwid_map.get(&relay_msg.recipient_id) {
                                Some(hw_id) => hw_id.value().clone(),
                                None => {
                                    // If recipient doesn't exist, we'll still try to queue the message
                                    // It might be registered later
                                    debug!("Recipient with ID {} not currently registered", relay_msg.recipient_id);
                                    String::new()
                                }
                            };
                            
                            if !recipient_hw_id.is_empty() {
                                // Check if devices are authorized
                                let authorized = match authorized_devices.get(&hardware_id) {
                                    Some(map) => map.contains_key(&recipient_hw_id),
                                    None => false
                                };
                                
                                if !authorized {
                                    warn!("Unauthorized message from {} to {}", hardware_id, recipient_hw_id);
                                    let error_msg = WebSocketMessage::Error {
                                        message: "Devices not paired".to_string(),
                                        code: Some(403),
                                    };
                                    let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    continue;
                                }
                            }
                        }
                    
                        
                        // Check if hardware ID has enough points for this message size
                        if let Some(mut rate_limit) = hardware_rate_limits.get_mut(&hardware_id) {
                            match rate_limit.check_and_consume(relay_msg.size_bytes) {
                                Ok(_) => {
                                    // Points successfully consumed
                                    debug!(
                                        "Message {} accepted. Size: {}KB", 
                                        relay_msg.message_id, 
                                        (relay_msg.size_bytes / 1024) + 1
                                    );
                                },
                                Err(limit_message) => {
                                    let error_msg = WebSocketMessage::Error {
                                        message: limit_message,
                                        code: Some(429),
                                    };
                                    let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    continue;
                                }
                            }
                        } else {
                            // Initialize rate limits if not exists
                            hardware_rate_limits.insert(hardware_id.clone(), RateLimit::default());
                            // Re-attempt consumption
                            if let Some(mut rate_limit) = hardware_rate_limits.get_mut(&hardware_id) {
                                if let Err(limit_message) = rate_limit.check_and_consume(relay_msg.size_bytes) {
                                    let error_msg = WebSocketMessage::Error {
                                        message: limit_message,
                                        code: Some(429),
                                    };
                                    let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    continue;
                                }
                            }
                        }

                        // Track outgoing bandwidth for the relay operation
                        if !bandwidth_tracker.add_bytes(relay_msg.size_bytes) {
                            error!("Monthly bandwidth limit approaching (9.5TB/10TB). Message relay rejected.");
                            let error_msg = WebSocketMessage::Error {
                                message: "Server has reached monthly bandwidth limit. Please try again next month.".to_string(),
                                code: Some(509), // 509 Bandwidth Limit Exceeded
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!(
                            "Relaying message {} from {} to {}",
                            relay_msg.message_id, relay_msg.sender_id, relay_msg.recipient_id
                        );
                        
                        // Make sure sender ID matches the registered device ID
                        if relay_msg.sender_id != device_id {
                            warn!("Sender ID mismatch: {} vs {}", relay_msg.sender_id, device_id);
                            relay_msg.sender_id = device_id.clone();
                        }
                        
                        // Check if recipient is connected by looking up its device ID
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&relay_msg.recipient_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Relay the message to the recipient
                                let forward_msg = WebSocketMessage::Receive(relay_msg.clone());
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&forward_msg).unwrap(),
                                )) {
                                    Ok(_) => {
                                        info!("Message {} delivered successfully", relay_msg.message_id);
                                        // Send acknowledgment to sender
                                        let ack = WebSocketMessage::Ack {
                                            message_id: relay_msg.message_id.to_string(),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&ack).unwrap()));
                                        
                                        // Update last active time for recipient
                                        if let Some(mut entry) = last_active.get_mut(&recipient_conn_id) {
                                            *entry = Instant::now();
                                        }
                                    }
                                    Err(e) => {
                                        error!("Failed to deliver message: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(
                                            serde_json::to_string(&error_msg).unwrap(),
                                        ));
                                    }
                                }
                            } else {
                                // Recipient not connected, queue message if TTL allows
                                if relay_msg.ttl > 0 {
                                    debug!("Queueing message for offline recipient: {}", relay_msg.recipient_id);
                                    
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
                                    let _ = tx.send(Message::text(serde_json::to_string(&ack).unwrap()));
                                } else {
                                    // No queuing requested
                                    let error_msg = WebSocketMessage::Error {
                                        message: "Recipient not connected".to_string(),
                                        code: Some(404),
                                    };
                                    let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                }
                            }
                        } else {
                            // Recipient not connected, queue message if TTL allows
                            if relay_msg.ttl > 0 {
                                debug!("Queueing message for offline recipient: {}", relay_msg.recipient_id);
                                
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
                                let _ = tx.send(Message::text(serde_json::to_string(&ack).unwrap()));
                            } else {
                                // No queuing requested
                                let error_msg = WebSocketMessage::Error {
                                    message: "Recipient not connected".to_string(),
                                    code: Some(404),
                                };
                                let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            }
                        }
                    },
                    
                    WebSocketMessage::AuthRequest { 
                        device_id: sender_id, 
                        public_key, 
                        challenge, 
                        hardware_id: sender_hw_id 
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in auth request: expected {}, got {}", hardware_id, sender_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Auth request from {} to {}", device_id, sender_id);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&sender_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward auth request
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::AuthRequest {
                                        device_id: device_id.clone(),
                                        public_key,
                                        challenge,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        debug!("Auth request forwarded to {}", sender_id);
                                    },
                                    Err(e) => {
                                        error!("Failed to forward auth request: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    }
                                }
                            }
                        } else {
                            // Recipient not connected
                            let error_msg = WebSocketMessage::Error {
                                message: "Recipient not connected".to_string(),
                                code: Some(404),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::AuthResponse { 
                        device_id: responder_id, 
                        challenge_response, 
                        challenge, 
                        hardware_id: responder_hw_id 
                    } => {
                        // Verify hardware ID matches the registered one
                        if responder_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in auth response: expected {}, got {}", hardware_id, responder_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Auth response from {} to {}", device_id, responder_id);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&responder_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward auth response
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::AuthResponse {
                                        device_id: device_id.clone(),
                                        challenge_response,
                                        challenge,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        debug!("Auth response forwarded to {}", responder_id);
                                    },
                                    Err(e) => {
                                        error!("Failed to forward auth response: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    }
                                }
                            }
                        } else {
                            // Recipient not connected
                            let error_msg = WebSocketMessage::Error {
                                message: "Recipient not connected".to_string(),
                                code: Some(404),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::AuthVerify { 
                        device_id: verifier_id, 
                        challenge_response,
                        hardware_id: verifier_hw_id 
                    } => {
                        // Verify hardware ID matches the registered one
                        if verifier_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in auth verify: expected {}, got {}", hardware_id, verifier_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Auth verification from {} to {}", device_id, verifier_id);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&verifier_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward auth verification
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::AuthVerify {
                                        device_id: device_id.clone(),
                                        challenge_response,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        debug!("Auth verification forwarded to {}", verifier_id);
                                    },
                                    Err(e) => {
                                        error!("Failed to forward auth verification: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    }
                                }
                            }
                        } else {
                            // Recipient not connected
                            let error_msg = WebSocketMessage::Error {
                                message: "Recipient not connected".to_string(),
                                code: Some(404),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::AuthSuccess { 
                        device_id: success_id, 
                        trusted, 
                        hardware_id: success_hw_id 
                    } => {
                        // Verify hardware ID matches the registered one
                        if success_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in auth success: expected {}, got {}", hardware_id, success_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Auth success from {} to {}, trusted: {}", device_id, success_id, trusted);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&success_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        // If trusted, update authorized devices
                        if trusted {
                            // Get hardware ID of the target device
                            if let Some(trusted_hw_id) = device_to_hwid_map.get(&success_id) {
                                // Add bidirectional trust
                                authorized_devices
                                    .entry(hardware_id.clone())
                                    .or_insert_with(DashMap::new)
                                    .insert(trusted_hw_id.value().clone(), true);
                                    
                                authorized_devices
                                    .entry(trusted_hw_id.value().clone())
                                    .or_insert_with(DashMap::new)
                                    .insert(hardware_id.clone(), true);
                                    
                                info!("Devices paired: {} <-> {}", hardware_id, trusted_hw_id.value());
                            }
                        }
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward auth success
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::AuthSuccess {
                                        device_id: device_id.clone(),
                                        trusted,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        debug!("Auth success forwarded to {}", success_id);
                                    },
                                    Err(e) => {
                                        error!("Failed to forward auth success: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    }
                                }
                            }
                        } else {
                            // Recipient not connected
                            let error_msg = WebSocketMessage::Error {
                                message: "Recipient not connected".to_string(),
                                code: Some(404),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::KeyRotationUpdate { 
                        sender_id, 
                        recipient_id, 
                        encrypted_key_package, 
                        key_id,
                        hardware_id: sender_hw_id
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in key rotation: expected {}, got {}", hardware_id, sender_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        // Verify sender is authorized to talk to recipient
                        let recipient_hw_id = match device_to_hwid_map.get(&recipient_id) {
                            Some(hw_id) => hw_id.value().clone(),
                            None => {
                                let error_msg = WebSocketMessage::Error {
                                    message: "Recipient not found".to_string(),
                                    code: Some(404),
                                };
                                let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                continue;
                            }
                        };
                        
                        let authorized = match authorized_devices.get(&hardware_id) {
                            Some(map) => map.contains_key(&recipient_hw_id),
                            None => false
                        };
                        
                        if !authorized {
                            warn!("Unauthorized key rotation from {} to {}", hardware_id, recipient_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Devices not paired".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Key rotation update from {} to {}", sender_id, recipient_id);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&recipient_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward the rotated key
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::KeyRotationUpdate {
                                        sender_id: sender_id.clone(),
                                        recipient_id: recipient_id.clone(),
                                        encrypted_key_package: encrypted_key_package.clone(),
                                        key_id,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        info!("Key rotation update delivered successfully");
                                    },
                                    Err(e) => {
                                        error!("Failed to deliver key rotation update: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable for key rotation".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                                    }
                                }
                            }
                        } else {
                            // Queue key rotation update with high priority, if offline
                            debug!("Queueing key rotation for offline recipient: {}", recipient_id);
                            
                            let queued_msg = QueuedMessage::new(
                                WebSocketMessage::KeyRotationUpdate {
                                    sender_id: sender_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    encrypted_key_package: encrypted_key_package.clone(),
                                    key_id,
                                    hardware_id: hardware_id.clone(),
                                },
                                300, // 5 minutes TTL for key updates
                            );
                            
                            message_queues
                                .entry(recipient_id.clone())
                                .or_insert_with(Vec::new)
                                .push(queued_msg);
                            
                            // Notify sender that message is queued
                            let info_msg = WebSocketMessage::Error {
                                message: "Key rotation queued for offline recipient".to_string(),
                                code: Some(202), // Accepted but not processed
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&info_msg).unwrap()));
                        }
                    },
                    
                    WebSocketMessage::KeyRotationAck { 
                        sender_id, 
                        recipient_id, 
                        key_id, 
                        success,
                        hardware_id: sender_hw_id
                    } => {
                        // Verify hardware ID matches the registered one
                        if sender_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in key rotation ack: expected {}, got {}", hardware_id, sender_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        info!("Key rotation acknowledgment from {} to {} for key {}", sender_id, recipient_id, key_id);
                        
                        // Find recipient's connection
                        let recipient_conn_id = if let Some(conn_id) = active_connections.get(&recipient_id) {
                            conn_id.value().clone()
                        } else {
                            String::new()
                        };
                        
                        if !recipient_conn_id.is_empty() {
                            if let Some(recipient_tx) = connections.get(&recipient_conn_id) {
                                // Forward key rotation acknowledgment
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&WebSocketMessage::KeyRotationAck {
                                        sender_id: sender_id.clone(),
                                        recipient_id: recipient_id.clone(),
                                        key_id,
                                        success,
                                        hardware_id: hardware_id.clone(),
                                    }).unwrap(),
                                )) {
                                    Ok(_) => {
                                        debug!("Key rotation ACK forwarded to {}", recipient_id);
                                    },
                                    Err(e) => {
                                        error!("Failed to forward key rotation ACK: {}", e);
                                    }
                                }
                            }
                        } else {
                            // Queue the ACK
                            debug!("Queueing key rotation ACK for offline recipient: {}", recipient_id);
                            
                            let queued_msg = QueuedMessage::new(
                                WebSocketMessage::KeyRotationAck {
                                    sender_id: sender_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    key_id,
                                    success,
                                    hardware_id: hardware_id.clone(),
                                },
                                300, // 5 minutes TTL
                            );
                            
                            message_queues
                                .entry(recipient_id.clone())
                                .or_insert_with(Vec::new)
                                .push(queued_msg);
                        }
                    },
                    
                    WebSocketMessage::Status { 
                        device_id: status_device_id, 
                        status,
                        hardware_id: status_hw_id 
                    } => {
                        // Verify hardware ID matches the registered one
                        if status_hw_id != hardware_id {
                            warn!("Hardware ID mismatch in status update: expected {}, got {}", hardware_id, status_hw_id);
                            let error_msg = WebSocketMessage::Error {
                                message: "Hardware ID mismatch".to_string(),
                                code: Some(403),
                            };
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                        
                        // Update device status (
                        if status_device_id == device_id {
                            debug!("Device {} status changed to {:?}", device_id, status);
                            
                            // If going offline, clean up resources
                            if status == DeviceStatus::Offline {
                                connections.remove(&connection_id);
                                last_active.remove(&connection_id);
                                active_connections.remove(&device_id);
                                
                                // Dont remove hardware ID mapping
                            }
                        }
                    },
                    
                    // Keep the websocket connection alive
                    WebSocketMessage::Ping => {
                        let pong = WebSocketMessage::Pong;
                        let _ = tx.send(Message::text(serde_json::to_string(&pong).unwrap()));
                    },
                    
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

    // Remove device from active connections but keep the hardware ID mapping
    if !device_id.is_empty() {
        active_connections.remove(&device_id);
    }
    
    info!("WebSocket connection closed for device: {}", device_id);
}