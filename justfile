# walkmap task runner

# build the pipeline binary
build:
    cd pipeline && cargo build --release

# run all stages for one city
run city: build
    ./pipeline/target/release/walkmap run {{city}}

# run both pilot cities
pilot: (run "funchal") (run "london")

# unit tests
test:
    cd pipeline && cargo test

# serve the web UI locally (tiles from data/out via ?local=1)
serve:
    python3 -m http.server 8000

# create the GitHub release for a city and upload its PMTiles
release city:
    ./scripts/release.sh {{city}}
