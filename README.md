## Info
This fork fo cuses on expanding the Quiche sample HTTP server implementation with the following features:
- Byte range support
- Mock endpoint to allow PRIORITY_UPDATE coming from browser clients (as Fetch API does not expose this)
- Add priority scheduler based on RFC 9218: Extensible Prioritization Scheme for HTTP


 ## Some common commands:
 ### Ensure BoringSSL is present.
 ```
 git submodule update --init --recursive
 ```

### Ensure the image is built.
```
docker build -t quiche-shaped -f Dockerfile .
```

### Running the server can be done in two ways:
#### Manually:
```
docker run --rm -it --cap-add NET_ADMIN -p 4433:4433/udp -e SHAPE=on -e IFACE=eth0 -e RATE=5mbit -e LAT=80ms -e JIT=20ms -e LOSS=0.5% quiche-shaped quiche-server --listen 0.0.0.0:4433 --root /www --cert /certs/cert.pem --key /certs/priv.key --disable-gso
```
#### Automatic script
On Windows, open Git Bash terminal in the root to do this easily.
```
sh run_server.sh
```