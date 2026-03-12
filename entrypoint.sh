#!/bin/bash
set -e

# Wait for PostgreSQL to be ready
MAX_ATTEMPTS=30
ATTEMPT=0
until python -c "
from sqlalchemy import create_engine, text
import os
engine = create_engine(os.environ['DATABASE_URL'])
with engine.connect() as conn:
    conn.execute(text('SELECT 1'))
" 2>/dev/null; do
    ATTEMPT=$((ATTEMPT + 1))
    if [ "$ATTEMPT" -ge "$MAX_ATTEMPTS" ]; then
        echo "Database not ready after $MAX_ATTEMPTS attempts, exiting."
        exit 1
    fi
    echo "Waiting for database... (attempt $ATTEMPT/$MAX_ATTEMPTS)"
    sleep 2
done
echo "Database is ready."

echo "Running database migrations..."
alembic upgrade head

echo "Starting application..."
exec "$@"
