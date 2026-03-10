"""add missing foreign key indexes for cascade delete performance

Revision ID: 2d932114930d
Revises: 8e71f2a4c9b0
Create Date: 2026-03-07 16:28:48.633531+00:00

"""
from alembic import op
from sqlalchemy import inspect

# revision identifiers, used by Alembic.
revision = '2d932114930d'
down_revision = '8e71f2a4c9b0'
branch_labels = None
depends_on = None


def _index_exists(table_name: str, index_name: str) -> bool:
    bind = op.get_bind()
    insp = inspect(bind)
    return any(idx["name"] == index_name for idx in insp.get_indexes(table_name))


def _create_index_if_not_exists(index_name: str, table_name: str, columns: list[str]) -> None:
    if not _index_exists(table_name, index_name):
        op.create_index(op.f(index_name), table_name, columns, unique=False)


def _drop_index_if_exists(index_name: str, table_name: str) -> None:
    if _index_exists(table_name, index_name):
        op.drop_index(op.f(index_name), table_name=table_name)


# (index_name, table_name, columns)
_INDEXES = [
    # api_keys.user_id (CASCADE -> users.id)
    ('ix_api_keys_user_id', 'api_keys', ['user_id']),
    # usage: wallet_id, provider_endpoint_id, provider_api_key_id (SET NULL)
    ('ix_usage_wallet_id', 'usage', ['wallet_id']),
    ('ix_usage_provider_endpoint_id', 'usage', ['provider_endpoint_id']),
    ('ix_usage_provider_api_key_id', 'usage', ['provider_api_key_id']),
    # wallet_transactions.operator_id (SET NULL -> users.id)
    ('ix_wallet_transactions_operator_id', 'wallet_transactions', ['operator_id']),
    # payment_callbacks.payment_order_id (SET NULL -> payment_orders.id)
    ('ix_payment_callbacks_payment_order_id', 'payment_callbacks', ['payment_order_id']),
    # refund_requests: payment_order_id, requested_by, approved_by, processed_by (SET NULL)
    ('ix_refund_requests_payment_order_id', 'refund_requests', ['payment_order_id']),
    ('ix_refund_requests_requested_by', 'refund_requests', ['requested_by']),
    ('ix_refund_requests_approved_by', 'refund_requests', ['approved_by']),
    ('ix_refund_requests_processed_by', 'refund_requests', ['processed_by']),
    # proxy_nodes.registered_by (SET NULL -> users.id)
    ('ix_proxy_nodes_registered_by', 'proxy_nodes', ['registered_by']),
    # video_tasks: api_key_id, provider_id, endpoint_id, key_id, remixed_from_task_id
    ('ix_video_tasks_api_key_id', 'video_tasks', ['api_key_id']),
    ('ix_video_tasks_provider_id', 'video_tasks', ['provider_id']),
    ('ix_video_tasks_endpoint_id', 'video_tasks', ['endpoint_id']),
    ('ix_video_tasks_key_id', 'video_tasks', ['key_id']),
    ('ix_video_tasks_remixed_from_task_id', 'video_tasks', ['remixed_from_task_id']),
    # user_preferences.default_provider_id (-> providers.id)
    ('ix_user_preferences_default_provider_id', 'user_preferences', ['default_provider_id']),
    # announcements.author_id (SET NULL -> users.id)
    ('ix_announcements_author_id', 'announcements', ['author_id']),
    # announcement_reads.announcement_id (-> announcements.id)
    ('ix_announcement_reads_announcement_id', 'announcement_reads', ['announcement_id']),
    # request_candidates: user_id, api_key_id, endpoint_id, key_id (CASCADE)
    ('ix_request_candidates_user_id', 'request_candidates', ['user_id']),
    ('ix_request_candidates_api_key_id', 'request_candidates', ['api_key_id']),
    ('ix_request_candidates_endpoint_id', 'request_candidates', ['endpoint_id']),
    ('ix_request_candidates_key_id', 'request_candidates', ['key_id']),
]


def upgrade() -> None:
    for index_name, table_name, columns in _INDEXES:
        _create_index_if_not_exists(index_name, table_name, columns)


def downgrade() -> None:
    for index_name, table_name, _columns in reversed(_INDEXES):
        _drop_index_if_exists(index_name, table_name)
