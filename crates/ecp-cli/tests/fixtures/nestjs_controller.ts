import { Controller, Get, Post, Delete, Param, Body } from "@nestjs/common";

@Controller("users")
export class UsersController {
    constructor(private readonly svc: UsersService) {}

    @Get(":id")
    findOne(@Param("id") id: string) {
        return this.svc.findOne(id);
    }

    @Get()
    findAll() {
        return this.svc.findAll();
    }

    @Post()
    create(@Body() dto: any) {
        return this.svc.create(dto);
    }

    @Delete(":id")
    remove(@Param("id") id: string) {
        return this.svc.remove(id);
    }
}

// 對照組：class 沒有 @Controller，方法上的 @Get 不該被抓
export class NotAController {
    @Get(":id")
    notARoute(id: string) { return id; }
}
