### 1. 创建集群项目

### 2. 添加两个数据库服务

   - redis (复制 Redis Connection String)
   - postgresql (复制 Connection String)

### 3. 添加Docker容器镜像

1. 镜像: ghcr.io/Shirtiny/aether:latest

2. 环境变量

   ```
   DATABASE_URL=(postgresql复制的内容)
   REDIS_URL=(redis复制的内容)
   JWT_SECRET_KEY=change-this-to-a-secure-random-string
   ENCRYPTION_KEY=change-this-to-another-secure-random-string
   ADMIN_EMAIL=admin@example.com
   ADMIN_USERNAME=admin
   ADMIN_PASSWORD=admin123456
   ```
  
  JWT_SECRET_KEY、ENCRYPTION_KEY: 可以运行项目中的python脚本自动生成 python generate_keys.py

  ADMIN_EMAIL、ADMIN_USERNAME、ADMIN_PASSWORD: 管理员初始信息必须修改

3. 端口: 80