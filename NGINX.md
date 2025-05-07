# Nginx Configuration Guide
A guide to set up Nginx as a secure reverse proxy for OpenRelay-Server

## Prerequisites

- Ubuntu/Debian (Or a similar linux distribution)
- Nginx  (`sudo apt install nginx`)
- Let's Encrypt certificates (or any other SSL certificate)
- A domain pointing to your server (e.g., relay.yourdomain.com)
- The OpenRelay server running on port 3000

## Configuration Files
### 1. Main Nginx config
Ensure it has the necessary websocket and rate limiting settings
Edit `/etc/nginx/nginx.conf` and add the following in `http`

```nginx
# WebSocket support
map $http_upgrade $connection_upgrade {
    default   upgrade;
    ''        close;
}
```

### 2. Default server config
Create or edit `/etc/nginx/sites-available/default`

```nginx
server {
    listen 80 default_server;
    listen [::]:80 default_server;
    return 444;
}
server {
    listen 443 ssl default_server;
    listen [::]:443 ssl default_server;
    
    ssl_certificate /etc/letsencrypt/live/relay.yourdomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.yourdomain.com/privkey.pem;

    return 444;
}
```

This sets it so that any direct requests to your server will be closed. If you do not want to close and instead redirect, then replace `return 444;` with `return 301 https://relay.yourdomain.com$request_uri;`

### 3. OpenRelay server config
Create or edit `/etc/nginx/sites-available/relay.yourdomain.com`

```nginx
##
# HTTP → HTTPS + ACME challenge
##
server {
    listen 80;
    listen [::]:80;
    server_name relay.yourdomain.com;
    
    # Allow ACME challenge for Let's Encrypt renewals
    location ^~ /.well-known/acme-challenge/ {
        root /var/www/html;
    }
    
    # Redirect all other HTTP traffic to HTTPS
    location / {
        return 301 https://$host$request_uri;
    }
}

##
# HTTPS + WebSocket + health proxy
##
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    server_name relay.yourdomain.com;
    
    # SSL configuration
    ssl_certificate     /etc/letsencrypt/live/relay.yourdomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/relay.yourdomain.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers on;
    ssl_ciphers         'ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384';
    ssl_session_cache   shared:SSL:10m;
    ssl_session_timeout 1d;
    
    # SECURITY HEADERS
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains" always;
    add_header X-Content-Type-Options nosniff always;
    add_header X-Frame-Options DENY always;
    add_header Content-Security-Policy "default-src 'self';" always;
    add_header Referrer-Policy strict-origin-when-cross-origin always;
    
    # BUFFER SIZE LIMITS
    client_body_buffer_size 10K;
    client_header_buffer_size 1k;
    client_max_body_size 8m;  # Increased to handle larger clipboard data
    large_client_header_buffers 2 1k;
    
    # Health endpoint
    location = /health {
        proxy_pass       http://127.0.0.1:3000/health;
        proxy_set_header Host $host;
    }
    
    # Bandwidth-status endpoint
    location = /bandwidth-status {
        proxy_pass       http://127.0.0.1:3000/bandwidth-status;
        proxy_set_header Host $host;
    }
    
    # WEBSOCKET: match anything under /relay
    location /relay {
        proxy_pass         http://127.0.0.1:3000;
        proxy_http_version 1.1;
        
        # Forward the upgrade headers
        proxy_set_header   Upgrade                $http_upgrade;
        proxy_set_header   Connection             $connection_upgrade;
        proxy_set_header   Host                   $host;
        proxy_set_header   X-Forwarded-Proto      $scheme;
        proxy_set_header   Sec-WebSocket-Key      $http_sec_websocket_key;
        proxy_set_header   Sec-WebSocket-Version  $http_sec_websocket_version;
        proxy_set_header   Sec-WebSocket-Extensions $http_sec_websocket_extensions;
        
        # Timeouts
        proxy_read_timeout  300s;
        proxy_send_timeout  300s;
        proxy_connect_timeout 10s;
        proxy_cache_bypass  $http_upgrade;
    }
    
    # Everything else → drop with 444 (close connection without response)
    location / {
        return 444;
    }
}
```

### 4. Enable the configuration
```bash
sudo ln -s /etc/nginx/sites-available/relay.yourdomain.com /etc/nginx/sites-enabled/
sudo ln -s /etc/nginx/sites-available/default /etc/nginx/sites-enabled/
```

### 5. Test the configuration
```bash
sudo nginx -t
```

### 6. Reload Nginx
If the test is successful, reload Nginx to apply the changes:
```bash
sudo systemctl reload nginx
```
