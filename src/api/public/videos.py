"""
Video Generation API 路由
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Depends, Request
from sqlalchemy.orm import Session

from src.api.base.pipeline import get_pipeline
from src.api.handlers.gemini.video_adapter import GeminiVeoAdapter
from src.api.handlers.openai.video_adapter import OpenAIVideoAdapter
from src.database import get_db

router = APIRouter(tags=["Video Generation"])
pipeline = get_pipeline()


# -------------------- OpenAI Sora compatible --------------------


@router.post("/v1/videos")
async def create_video_sora(http_request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
    )


@router.post("/v1/videos/{task_id}/cancel")
async def cancel_video_sora(
    task_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    """Cancel video task (OpenAI Sora style)."""
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": task_id, "action": "cancel"},
    )


@router.get("/v1/videos/{task_id}")
async def get_video_task_sora(
    task_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": task_id},
    )


@router.get("/v1/videos")
async def list_video_tasks_sora(http_request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
    )


@router.delete("/v1/videos/{task_id}")
async def delete_video_task_sora(
    task_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    """删除已完成或失败的视频及其存储资源"""
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": task_id},
    )


@router.get("/v1/videos/{task_id}/content")
async def download_video_content_sora(
    task_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": task_id},
    )


@router.post("/v1/videos/{task_id}/remix")
async def remix_video_sora(
    task_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = OpenAIVideoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": task_id},
    )


# -------------------- Gemini Veo compatible --------------------


@router.post("/v1beta/models/{model}:predictLongRunning")
async def create_video_veo(model: str, http_request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = GeminiVeoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"model": model},
    )


# Gemini Veo operation routes - support both formats:
# 1. models/{model}/operations/{id} (official Gemini Veo format)
# 2. operations/{...} (legacy format for compatibility)


@router.get("/v1beta/models/{model}/operations/{operation_id}")
async def get_video_veo_by_model(
    model: str, operation_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    """Get video task status (Gemini Veo format: models/{model}/operations/{id})"""
    adapter = GeminiVeoAdapter()
    # Reconstruct full operation name
    full_operation_name = f"models/{model}/operations/{operation_id}"
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": full_operation_name},
    )


@router.post("/v1beta/models/{model}/operations/{operation_id}:cancel")
async def cancel_video_veo_by_model(
    model: str, operation_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    """Cancel video task (Gemini Veo format: models/{model}/operations/{id}:cancel)"""
    adapter = GeminiVeoAdapter()
    full_operation_name = f"models/{model}/operations/{operation_id}"
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": full_operation_name, "action": "cancel"},
    )


# Legacy routes for backward compatibility
@router.get("/v1beta/operations/{operation_id:path}")
async def get_video_veo(
    operation_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = GeminiVeoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": operation_id},
    )


@router.get("/v1beta/operations")
async def list_video_tasks_veo(http_request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = GeminiVeoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
    )


@router.post("/v1beta/operations/{operation_id}:cancel")
async def cancel_video_veo(
    operation_id: str, http_request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = GeminiVeoAdapter()
    return await pipeline.run(
        adapter=adapter,
        http_request=http_request,
        db=db,
        mode=adapter.mode,
        api_format_hint=adapter.allowed_api_formats[0],
        path_params={"task_id": operation_id, "action": "cancel"},
    )


# Video download is now handled by /v1beta/files/{task_id}:download in gemini_files.py
