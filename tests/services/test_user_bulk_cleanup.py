from __future__ import annotations

from sqlalchemy import Column, Integer, String, create_engine
from sqlalchemy.orm import Session, declarative_base, sessionmaker

from src.services.user.bulk_cleanup import batch_nullify_fk

Base = declarative_base()


class DemoRow(Base):
    __tablename__ = "demo_rows"

    id = Column(Integer, primary_key=True, autoincrement=True)
    ref_id = Column(String(36), nullable=True)


def test_batch_nullify_fk_handles_sqlite_batches() -> None:
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    session_factory = sessionmaker(bind=engine)
    db: Session = session_factory()

    try:
        db.add_all([DemoRow(ref_id="entity-1") for _ in range(905)])
        db.add_all([DemoRow(ref_id="entity-2") for _ in range(3)])
        db.commit()

        updated = batch_nullify_fk(db, DemoRow, "ref_id", "entity-1")

        assert updated == 905
        assert db.query(DemoRow).filter(DemoRow.ref_id.is_(None)).count() == 905
        assert db.query(DemoRow).filter(DemoRow.ref_id == "entity-2").count() == 3
    finally:
        db.close()
