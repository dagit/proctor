# About

`proctor` will proxy your HTTP traffic for you. All it needs is a list
of allowed remote servers and a port to listen on.

This is really just a simple and quick proof of concept I made to
learn how HTTP proxies work. It's not heavily tested, featureful, or
otherwise ready for serious use. I did all the HTTP parsing and other
bits myself to see how they work. I would recommend using the
appropriate libraries instead.

# Usage

```
proctor

USAGE:
    proctor.exe [FLAGS] --port <port> [HOSTNAME]...

ARGS:
    <HOSTNAME>...    Remote servers that can be connected to on port 443

FLAGS:
    -d, --debug      Debug mode
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -p, --port <port>    Port to listen on
```

You must specify at least one `HOSTNAME`.

To see it work, you might run a test like this:

```sh
cargo run -- -p 8080 api.giphy.com
```

The above command tells your `proctor` to listen on localhost:8080 and
only allow proxy connections to `api.giphy.com`. Any other proxy
request would be denied.

Then in a different shell:
```sh
curl -x https:://localhost:8080 'http://api.giphy.com/v1/gifs/search?q=I&api_key=dc6zaTOxFJmzC'
```

However, this example will just result in an error unless you setup
cURL to work with giphy's api. For instance, you would need an API key
and other things.
