# walkmap task runner

# build the pipeline binary
build:
    cd pipeline && cargo build --release

# run all stages for one city
run city: build
    ./pipeline/target/release/walkmap run {{city}}

# rebuild + publish one city (or: just rebuild all)
rebuild city:
    ./scripts/rebuild.sh {{city}}

# feature-group research for a city (counts, grouping, spacing verdicts)
analyze city: build
    ./pipeline/target/release/walkmap analyze {{city}}

# unit tests
test:
    cd pipeline && cargo test

# serve the web UI locally (tiles from data/out via ?local=1)
serve:
    python3 -m http.server 8000

# create the GitHub release for a city and upload its PMTiles
release city:
    ./scripts/release.sh {{city}}
