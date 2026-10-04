#!/usr/bin/env python3
import argparse
import json
import re
import struct
import zipfile


class Class:
    def __init__(self, data):
        self.data, self.at = data, 8
        self.cp = [None] * self.u2()
        index = 1
        while index < len(self.cp):
            tag = self.u1()
            if tag == 1:
                value = self.take(self.u2()).decode('utf-8', errors='replace')
            elif tag in (7, 8, 16, 19, 20):
                value = self.u2()
            elif tag in (9, 10, 11, 12, 17, 18):
                value = (self.u2(), self.u2())
            elif tag == 3:
                value = struct.unpack('>i', self.take(4))[0]
            elif tag == 4:
                value = struct.unpack('>f', self.take(4))[0]
            elif tag == 5:
                value = struct.unpack('>q', self.take(8))[0]
            elif tag == 6:
                value = struct.unpack('>d', self.take(8))[0]
            elif tag == 15:
                value = (self.u1(), self.u2())
            else:
                raise ValueError(tag)
            self.cp[index] = (tag, value)
            index += 2 if tag in (5, 6) else 1
        self.at += 6
        interfaces = self.u2()
        self.at += 2 * interfaces
        self.fields, self.methods = {}, {}
        for group in (self.fields, self.methods):
            for _ in range(self.u2()):
                access, name, descriptor = self.u2(), self.resolve(self.u2()), self.resolve(self.u2())
                attributes = self.attributes()
                group[(name, descriptor)] = attributes
        self.class_attributes = self.attributes()
        self.bootstraps = []
        bootstrap = self.class_attributes.get('BootstrapMethods')
        if bootstrap:
            old_data, old_at = self.data, self.at
            self.data, self.at = bootstrap, 0
            for _ in range(self.u2()):
                handle = self.resolve(self.u2())
                args = [self.resolve(self.u2()) for _ in range(self.u2())]
                self.bootstraps.append((handle, args))
            self.data, self.at = old_data, old_at

    def take(self, count):
        result = self.data[self.at:self.at + count]
        if len(result) != count:
            raise ValueError('short class')
        self.at += count
        return result

    def u1(self):
        return self.take(1)[0]

    def u2(self):
        return int.from_bytes(self.take(2), 'big')

    def u4(self):
        return int.from_bytes(self.take(4), 'big')

    def attributes(self):
        result = {}
        for _ in range(self.u2()):
            name, count = self.resolve(self.u2()), self.u4()
            result[name] = self.take(count)
        return result

    def resolve(self, index):
        tag, value = self.cp[index]
        if tag in (1, 3, 4, 5, 6):
            return value
        if tag in (7, 8, 16, 19, 20):
            return self.resolve(value)
        if tag in (9, 10, 11, 12):
            return tuple(self.resolve(part) for part in value)
        if tag == 15:
            return ('handle', value[0], self.resolve(value[1]))
        if tag in (17, 18):
            return ('dynamic', value[0], self.resolve(value[1]))
        raise ValueError(tag)

    def instructions(self, attributes):
        if 'Code' not in attributes:
            return []
        body = attributes['Code']
        count = int.from_bytes(body[4:8], 'big')
        code = body[8:8 + count]
        at, out = 0, []
        names = {0x12:'ldc',0x13:'ldc_w',0x14:'ldc2_w',0xb2:'getstatic',0xb3:'putstatic',0xb4:'getfield',0xb5:'putfield',0xb6:'invokevirtual',0xb7:'invokespecial',0xb8:'invokestatic',0xb9:'invokeinterface',0xba:'invokedynamic',0xbb:'new',0xbd:'anewarray',0xc0:'checkcast',0xc1:'instanceof',0x10:'bipush',0x11:'sipush'}
        one = {0x10,0x12,0x15,0x16,0x17,0x18,0x19,0x36,0x37,0x38,0x39,0x3a,0xa9,0xbc}
        two = {0x11,0x13,0x14,0x84,*range(0x99,0xa9),*range(0xb2,0xb9),0xbb,0xbd,0xc0,0xc1,0xc6,0xc7}
        cp_two = {0x13,0x14,*range(0xb2,0xbb),0xbb,0xbd,0xc0,0xc1,0xc5}
        while at < len(code):
            start, opcode = at, code[at]
            at += 1
            if opcode == 0xaa:
                at += (-at) % 4
                low = int.from_bytes(code[at+4:at+8],'big',signed=True)
                high = int.from_bytes(code[at+8:at+12],'big',signed=True)
                at += 12 + (high-low+1)*4
            elif opcode == 0xab:
                at += (-at) % 4
                count = int.from_bytes(code[at+4:at+8],'big')
                at += 8 + count*8
            elif opcode == 0xc4:
                at += 5 if code[at] == 0x84 else 3
            else:
                at += 1 if opcode in one else 2 if opcode in two else 4 if opcode in (0xb9,0xba,0xc8,0xc9) else 3 if opcode == 0xc5 else 0
            operands = code[start+1:at]
            value = operands.hex()
            if opcode == 0x12:
                value = self.resolve(operands[0])
            elif opcode in cp_two:
                value = self.resolve(int.from_bytes(operands[:2],'big'))
                if opcode == 0xba:
                    value = (value[0], self.bootstraps[value[1]], value[2])
            out.append((start,names.get(opcode,f'{opcode:02x}'),value))
        return out

    def contract(self):
        return {name+descriptor:[(op,value) for _,op,value in self.instructions(attrs)] for (name,descriptor),attrs in self.methods.items()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('jar')
    parser.add_argument('class_name',nargs='?')
    parser.add_argument('--compare')
    parser.add_argument('--prefix',action='append',default=[])
    parser.add_argument('--method')
    parser.add_argument('--concise', action='store_true')
    args = parser.parse_args()
    with zipfile.ZipFile(args.jar) as jar:
        if args.class_name:
            cls = Class(jar.read(args.class_name+'.class'))
            for (name,descriptor),attrs in cls.methods.items():
                if args.method and not re.search(args.method, name+descriptor):
                    continue
                print(name,descriptor)
                for at,op,value in cls.instructions(attrs):
                    if args.concise and op not in ('getstatic','putstatic','invokevirtual','invokestatic','invokeinterface','invokespecial','ldc','ldc_w','bipush','sipush'):
                        continue
                    print(f'  {at:04x} {op} {value}')
        elif args.compare:
            with zipfile.ZipFile(args.compare) as base:
                names = sorted(name for name in jar.namelist() if name.endswith('.class') and any(name.startswith(p) for p in args.prefix))
                old_names = set(base.namelist())
                for name in names:
                    if name not in old_names:
                        print('ADDED',name)
                        continue
                    old, new = Class(base.read(name)).contract(), Class(jar.read(name)).contract()
                    changed = [key for key in new if key not in old or new[key] != old[key]]
                    removed = [key for key in old if key not in new]
                    if changed or removed:
                        print('CHANGED',name)
                        for key in changed:
                            print(' +',key)
                        for key in removed:
                            print(' -',key)


if __name__ == '__main__':
    main()
