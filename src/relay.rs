    use crate::types::{ BandwidthTracker, DeviceStatus, QueuedMessage, RateLimit, MessageQueues, LastActive, WebSocketMessage };
    use dashmap::DashMap;
    use futures_util::{ SinkExt, StreamExt };
    use std::{ net::SocketAddr, sync::Arc, time::{Duration, Instant} };
    use tokio::{ sync::mpsc, time };
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tracing::{ debug, error, info };
    use uuid::Uuid;
    use warp::{ ws::{Message, WebSocket}, Filter };

    // Store active connections with their device IDs and message channels
    // The server will use this to find the correct connection to send messages to
    type Connections = DashMap<String, mpsc::UnboundedSender<Message>>;

    // Store rate limits per device
    type RateLimits = DashMap<String, RateLimit>;

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
                queue_ttl: Duration::from_secs(30),          // 30 second TTL for queued messages
                bandwidth_tracker: BandwidthTracker::new("bandwidth_data.txt"),
            }
        }

        pub async fn run(&self) {
            // Create shared state
            let connections = Arc::new(Connections::new());
            let rate_limits = Arc::new(RateLimits::new());
            let message_queues = Arc::new(MessageQueues::new());
            let last_active = Arc::new(LastActive::new());
            let bandwidth_tracker = Arc::new(self.bandwidth_tracker.clone());

            // Create filters to pass state to handlers
            // Clone Arcs here so the originals are not moved into the closures
            let connections_clone = connections.clone();
            let rate_limits_clone = rate_limits.clone();
            let message_queues_clone = message_queues.clone();
            let last_active_clone = last_active.clone();
            let bandwidth_tracker_clone = bandwidth_tracker.clone();

            let connections_filter = warp::any().map(move || connections_clone.clone());
            let rate_limits_filter = warp::any().map(move || rate_limits_clone.clone());
            let message_queues_filter = warp::any().map(move || message_queues_clone.clone());
            let last_active_filter = warp::any().map(move || last_active_clone.clone());
            let bandwidth_tracker_filter = warp::any().map(move || bandwidth_tracker_clone.clone());

            // Define WebSocket route
            let ws_route = warp::path("relay")
                .and(warp::ws())
                .and(connections_filter.clone())
                .and(rate_limits_filter.clone())
                .and(message_queues_filter.clone())
                .and(last_active_filter.clone())
                .and(bandwidth_tracker_filter.clone())
                .map(|ws: warp::ws::Ws, connections, rate_limits, message_queues, last_active, bandwidth_tracker| {
                    ws.on_upgrade(move |socket| {
                        handle_websocket(
                            socket, 
                            connections, 
                            rate_limits, 
                            message_queues, 
                            last_active,
                            bandwidth_tracker,
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
        rate_limits: Arc<RateLimits>,
        message_queues: Arc<MessageQueues>,
        last_active: Arc<LastActive>,
        bandwidth_tracker: Arc<BandwidthTracker>,
    ) {
        let (mut ws_tx, mut ws_rx) = ws.split();

        // Generate a temporary ID until registration
        let mut device_id = Uuid::new_v4().to_string();
        info!("New WebSocket connection established (temp id: {})", device_id);

        // Initialize rate limit for this connection
        rate_limits.insert(device_id.clone(), RateLimit::default());
        
        // Set initial last active time
        last_active.insert(device_id.clone(), Instant::now());

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
        connections.insert(device_id.clone(), tx.clone());

        // Process incoming WebSocket messages
        while let Some(result) = ws_rx.next().await {
            match result {
                Ok(msg) => {
                    // Update last active time
                    if let Some(mut entry) = last_active.get_mut(&device_id) {
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
                            let _ = tx.send(Message::text(serde_json::to_string(&error_msg).unwrap()));
                            continue;
                        }
                    };

                    // Enforce rate limits with combined system
                    if let Some(mut rate_limit) = rate_limits.get_mut(&device_id) {
                        // Use a small fixed minimum cost for the message
                        match rate_limit.check_and_consume(1024) {
                            Ok(_) => {
                                // Limit check passed, proceed with message handling
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
                    }

                    match ws_message {
                        WebSocketMessage::Register => {
                            // Remove temporary connections
                            connections.remove(&device_id);
                            rate_limits.remove(&device_id);
                            last_active.remove(&device_id);
                            
                            // Generate a permanent device ID
                            device_id = Uuid::new_v4().to_string();
                            info!("Device registered with ID: {}", device_id);
                            
                            // Store with new device ID
                            connections.insert(device_id.clone(), tx.clone());
                            rate_limits.insert(device_id.clone(), RateLimit::default());
                            last_active.insert(device_id.clone(), Instant::now());
                            
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
                        }
                        
                        WebSocketMessage::Send(relay_msg) => {
                            // Check if device has enough points for this message size
                            if let Some(mut rate_limit) = rate_limits.get_mut(&device_id) {
                                match rate_limit.check_and_consume(relay_msg.size_bytes) {
                                    Ok(_) => {
                                        // Points successfully consumed
                                        info!(
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
                            
                            // Check if recipient is connected
                            if let Some(recipient_tx) = connections.get(&relay_msg.recipient_id) {
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
                                        if let Some(mut entry) = last_active.get_mut(&relay_msg.recipient_id) {
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
                        }
                        
                        WebSocketMessage::KeyRotationUpdate { sender_id, recipient_id, encrypted_key_package, key_id } => {
                            info!(
                                "Key rotation update from {} to {}",
                                sender_id, recipient_id
                            );
                            
                            // Check if recipient is connected
                            if let Some(recipient_tx) = connections.get(&recipient_id) {
                                // Forward the rotated key
                                let update_msg = WebSocketMessage::KeyRotationUpdate {
                                    sender_id: sender_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    encrypted_key_package: encrypted_key_package.clone(),
                                    key_id: key_id,
                                };
                                
                                match recipient_tx.send(Message::text(
                                    serde_json::to_string(&update_msg).unwrap(),
                                )) {
                                    Ok(_) => {
                                        info!("Key rotation update delivered successfully");
                                        // No immediate acknowledgment for key rotation
                                    }
                                    Err(e) => {
                                        error!("Failed to deliver key rotation update: {}", e);
                                        let error_msg = WebSocketMessage::Error {
                                            message: "Recipient unavailable for key rotation".to_string(),
                                            code: Some(503),
                                        };
                                        let _ = tx.send(Message::text(
                                            serde_json::to_string(&error_msg).unwrap(),
                                        ));
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
                                        key_id: key_id,
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
                        }
                        
                        WebSocketMessage::KeyRotationAck { sender_id, recipient_id, key_id, success } => {
                            info!(
                                "Key rotation acknowledgment from {} to {} for key {}",
                                sender_id, recipient_id, key_id
                            );
                            
                            // Forward acknowledgment to the key originator
                            if let Some(recipient_tx) = connections.get(&recipient_id) {
                                let ack_msg = WebSocketMessage::KeyRotationAck {
                                    sender_id: sender_id.clone(),
                                    recipient_id: recipient_id.clone(),
                                    key_id: key_id,
                                    success: success,
                                };
                                
                                let _ = recipient_tx.send(Message::text(
                                    serde_json::to_string(&ack_msg).unwrap(),
                                ));
                            }
                        }
                        
                        WebSocketMessage::Status { device_id: status_device_id, status } => {
                            // Update device status (useful for presence information)
                            if status_device_id == device_id {
                                debug!("Device {} status changed to {:?}", device_id, status);
                                
                                // If going offline, clean up resources
                                if status == DeviceStatus::Offline {
                                    connections.remove(&device_id);
                                    rate_limits.remove(&device_id);
                                    last_active.remove(&device_id);
                                }
                            }
                        }
                        
                        // Keep the websocket connection alive
                        WebSocketMessage::Ping => {
                            // Respond with pong
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
        connections.remove(&device_id);
        rate_limits.remove(&device_id);
        last_active.remove(&device_id);
        info!("WebSocket connection closed for device: {}", device_id);
    }